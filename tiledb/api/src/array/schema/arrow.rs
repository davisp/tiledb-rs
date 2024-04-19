use anyhow::anyhow;
use arrow_schema::Schema as ArrowSchema;
use serde::{Deserialize, Serialize};

use crate::array::{
    ArrayType, CellOrder, DomainBuilder, Schema, SchemaBuilder, TileOrder,
};
use crate::filter::arrow::FilterMetadata;
use crate::{error::Error, Context, Result as TileDBResult};

/// Represents required metadata to convert from an arrow schema
/// to a TileDB schema.
#[derive(Deserialize, Serialize)]
pub struct SchemaMetadata {
    array_type: ArrayType,
    version: i64,
    capacity: u64,
    allows_duplicates: bool,
    cell_order: CellOrder,
    tile_order: TileOrder,
    coordinate_filters: FilterMetadata,
    offsets_filters: FilterMetadata,
    nullity_filters: FilterMetadata,

    /// Number of dimensions in this schema. The first `ndim` Fields are
    /// Dimensions, not Attributes
    ndim: usize,
}

impl SchemaMetadata {
    pub fn new(schema: &Schema) -> TileDBResult<Self> {
        Ok(SchemaMetadata {
            array_type: schema.array_type()?,
            version: schema.version()?,
            capacity: schema.capacity()?,
            allows_duplicates: schema.allows_duplicates()?,
            cell_order: schema.cell_order()?,
            tile_order: schema.tile_order()?,
            coordinate_filters: FilterMetadata::new(
                &schema.coordinate_filters()?,
            )?,
            offsets_filters: FilterMetadata::new(&schema.offsets_filters()?)?,
            nullity_filters: FilterMetadata::new(&schema.nullity_filters()?)?,
            ndim: schema.domain()?.ndim()?,
        })
    }
}

pub fn arrow_schema<'ctx>(
    tiledb: &'ctx Schema<'ctx>,
) -> TileDBResult<Option<ArrowSchema>> {
    let mut builder =
        arrow_schema::SchemaBuilder::with_capacity(tiledb.nattributes()?);

    for d in 0..tiledb.domain()?.ndim()? {
        let dim = tiledb.domain()?.dimension(d)?;
        if let Some(field) = crate::array::dimension::arrow::arrow_field(&dim)?
        {
            builder.push(field)
        } else {
            return Ok(None);
        }
    }

    for a in 0..tiledb.nattributes()? {
        let attr = tiledb.attribute(a)?;
        if let Some(field) = crate::array::attribute::arrow::arrow_field(&attr)?
        {
            builder.push(field)
        } else {
            return Ok(None);
        }
    }

    let metadata = serde_json::ser::to_string(&SchemaMetadata::new(tiledb)?)
        .map_err(|e| {
            Error::Serialization(String::from("schema metadata"), anyhow!(e))
        })?;
    builder
        .metadata_mut()
        .insert(String::from("tiledb"), metadata);

    Ok(Some(builder.finish()))
}

/// Construct a TileDB schema from an Arrow schema.
/// A TileDB schema must have domain and dimension details.
/// These are expected to be in the schema `metadata` beneath the key `tiledb`.
/// This metadata is expected to be a JSON object with the following fields:
pub fn tiledb_schema<'ctx>(
    context: &'ctx Context,
    schema: &ArrowSchema,
) -> TileDBResult<Option<SchemaBuilder<'ctx>>> {
    let metadata = match schema.metadata().get("tiledb") {
        Some(metadata) => serde_json::from_str::<SchemaMetadata>(metadata)
            .map_err(|e| {
                Error::Deserialization(
                    String::from("schema metadata"),
                    anyhow!(e),
                )
            })?,
        None => return Ok(None),
    };

    if schema.fields.len() < metadata.ndim {
        return Err(Error::InvalidArgument(anyhow!(format!(
            "Expected at least {} dimension fields but only found {}",
            metadata.ndim,
            schema.fields.len()
        ))));
    }

    let dimensions = schema.fields.iter().take(metadata.ndim);
    let attributes = schema.fields.iter().skip(metadata.ndim);

    let domain = {
        let mut b = DomainBuilder::new(context)?;
        for f in dimensions {
            if let Some(dimension) =
                crate::array::dimension::arrow::tiledb_dimension(context, f)?
            {
                b = b.add_dimension(dimension.build())?;
            } else {
                return Ok(None);
            }
        }
        b.build()
    };

    let mut b = SchemaBuilder::new(context, metadata.array_type, domain)?
        .capacity(metadata.capacity)?
        .allow_duplicates(metadata.allows_duplicates)?
        .cell_order(metadata.cell_order)?
        .tile_order(metadata.tile_order)?
        .coordinate_filters(&metadata.coordinate_filters.create(context)?)?
        .offsets_filters(&metadata.offsets_filters.create(context)?)?
        .nullity_filters(&metadata.nullity_filters.create(context)?)?;

    for f in attributes {
        if let Some(attr) =
            crate::array::attribute::arrow::tiledb_attribute(context, f)?
        {
            b = b.add_attribute(attr.build())?;
        } else {
            return Ok(None);
        }
    }

    Ok(Some(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::schema::SchemaData;
    use crate::Factory;
    use proptest::prelude::*;

    #[test]
    fn test_tiledb_arrow_tiledb() -> TileDBResult<()> {
        let c: Context = Context::new()?;

        /* tiledb => arrow => tiledb */
        proptest!(|(tdb_in in any::<SchemaData>())| {
            let tdb_in = tdb_in.create(&c)
                .expect("Error constructing arbitrary tiledb attribute");
            if let Some(arrow_schema) = arrow_schema(&tdb_in)
                    .expect("Error reading tiledb schema") {
                // convert back to TileDB attribute
                let tdb_out = tiledb_schema(&c, &arrow_schema)?
                    .expect("Arrow schema did not invert")
                    .build().expect("Error creating TileDB schema");
                assert_eq!(tdb_in, tdb_out);
            }
        });

        Ok(())
    }
}