use std::ops::Deref;

use anyhow::anyhow;

use crate::array::Schema;
use crate::context::{CApiInterface, Context, ContextBound};
use crate::datatype::{LogicalType, PhysicalType};
use crate::error::{DatatypeErrorKind, Error};
use crate::key::LookupKey;
use crate::query::QueryBuilder;
use crate::range::{Range, SingleValueRange, TypedRange, VarValueRange};
use crate::Result as TileDBResult;
use crate::{fn_typed, single_value_range_go, var_value_range_go};

pub(crate) enum RawSubarray {
    Owned(*mut ffi::tiledb_subarray_t),
}

impl Deref for RawSubarray {
    type Target = *mut ffi::tiledb_subarray_t;
    fn deref(&self) -> &Self::Target {
        match *self {
            RawSubarray::Owned(ref ffi) => ffi,
        }
    }
}

impl Drop for RawSubarray {
    fn drop(&mut self) {
        let RawSubarray::Owned(ref mut ffi) = *self;
        unsafe { ffi::tiledb_subarray_free(ffi) };
    }
}

#[derive(ContextBound)]
pub struct Subarray<'ctx> {
    #[base(ContextBound)]
    schema: Schema<'ctx>,
    raw: RawSubarray,
}

impl<'ctx> Subarray<'ctx> {
    pub(crate) fn capi(&self) -> *mut ffi::tiledb_subarray_t {
        *self.raw
    }

    pub(crate) fn new(schema: Schema<'ctx>, raw: RawSubarray) -> Self {
        Subarray { schema, raw }
    }

    /// Return all dimension ranges set on the query.
    pub fn ranges(&self) -> TileDBResult<Vec<Vec<Range>>> {
        let c_subarray = self.capi();
        let ndims = self.schema.domain()?.ndim()? as u32;
        let mut ranges: Vec<Vec<Range>> = Vec::new();
        for dim_idx in 0..ndims {
            let mut nranges: u64 = 0;
            self.capi_call(|ctx| unsafe {
                ffi::tiledb_subarray_get_range_num(
                    ctx,
                    c_subarray,
                    dim_idx,
                    &mut nranges,
                )
            })?;

            let dim = self.schema.domain()?.dimension(dim_idx)?;
            let var_sized_dim = dim.is_var_sized()?;

            let mut dim_ranges: Vec<Range> = Vec::new();
            for rng_idx in 0..nranges {
                if var_sized_dim {
                    let mut start_size: u64 = 0;
                    let mut end_size: u64 = 0;
                    self.capi_call(|ctx| unsafe {
                        ffi::tiledb_subarray_get_range_var_size(
                            ctx,
                            c_subarray,
                            dim_idx,
                            rng_idx,
                            &mut start_size,
                            &mut end_size,
                        )
                    })?;

                    let start =
                        vec![0u8; start_size as usize].into_boxed_slice();
                    let end = vec![0u8; end_size as usize].into_boxed_slice();

                    self.capi_call(|ctx| unsafe {
                        ffi::tiledb_subarray_get_range_var(
                            ctx,
                            c_subarray,
                            dim_idx,
                            rng_idx,
                            start.as_ptr() as *mut std::ffi::c_void,
                            end.as_ptr() as *mut std::ffi::c_void,
                        )
                    })?;

                    let dtype = dim.datatype()?;
                    let cvn = dim.cell_val_num()?;
                    let range =
                        TypedRange::from_slices(dtype, cvn, &start, &end)?
                            .range;
                    dim_ranges.push(range);
                } else {
                    let dtype = dim.datatype()?;

                    // Apparently stride exists in the API but isn't used.
                    let mut stride: *const std::ffi::c_void = out_ptr!();

                    fn_typed!(dtype, LT, {
                        type DT = <LT as LogicalType>::PhysicalType;
                        let mut start_ptr: *const DT = out_ptr!();
                        let mut end_ptr: *const DT = out_ptr!();
                        self.capi_call(|ctx| unsafe {
                            ffi::tiledb_subarray_get_range(
                                ctx,
                                c_subarray,
                                dim_idx,
                                rng_idx,
                                &mut start_ptr as *mut *const DT
                                    as *mut *const std::ffi::c_void,
                                &mut end_ptr as *mut *const DT
                                    as *mut *const std::ffi::c_void,
                                &mut stride,
                            )
                        })?;

                        let (start, end) = unsafe { (*start_ptr, *end_ptr) };
                        let range = Range::from(&[start, end]);
                        dim_ranges.push(range);
                    })
                }
            }

            ranges.push(dim_ranges);
        }

        Ok(ranges)
    }
}

#[derive(ContextBound)]
pub struct Builder<Q> {
    #[base(ContextBound)]
    query: Q,
    raw: RawSubarray,
}

impl<'ctx, Q> Builder<Q>
where
    Q: QueryBuilder<'ctx> + Sized,
{
    pub(crate) fn for_query(query: Q) -> TileDBResult<Self> {
        let context = query.base().context();
        let c_array = **query.base().carray();
        let mut c_subarray: *mut ffi::tiledb_subarray_t = out_ptr!();

        context.capi_call(|ctx| unsafe {
            ffi::tiledb_subarray_alloc(ctx, c_array, &mut c_subarray)
        })?;

        Ok(Builder {
            query,
            raw: RawSubarray::Owned(c_subarray),
        })
    }

    /// Add a range on a dimension to the subarray. Adding a range restricts
    /// how much data TileDB has to read from disk to complete a query.
    pub fn add_range<Key: Into<LookupKey> + Clone, IntoRange: Into<Range>>(
        self,
        key: Key,
        range: IntoRange,
    ) -> TileDBResult<Self> {
        // Get the dimension so that we can assert the correct Range type.
        let schema = self.query.base().query.array.schema()?;
        let dim = schema.domain()?.dimension(key.clone())?;

        let range = range.into();
        range
            .check_dimension_compatibility(dim.datatype()?, dim.cell_val_num()?)
            .map_err(|e| {
                Error::InvalidArgument(
                    anyhow!("Invalid range variant for dimension").context(e),
                )
            })?;

        let c_subarray = *self.raw;

        match range {
            Range::Single(range) => {
                single_value_range_go!(range, _DT, start, end, {
                    let start = start.to_le_bytes();
                    let end = end.to_le_bytes();
                    match key.into() {
                        LookupKey::Index(idx) => {
                            self.query.base().capi_call(|ctx| unsafe {
                                ffi::tiledb_subarray_add_range(
                                    ctx,
                                    c_subarray,
                                    idx as u32,
                                    start.as_ptr() as *const std::ffi::c_void,
                                    end.as_ptr() as *const std::ffi::c_void,
                                    std::ptr::null(),
                                )
                            })?;
                        }
                        LookupKey::Name(name) => {
                            let c_name = cstring!(name);
                            self.query.base().capi_call(|ctx| unsafe {
                                ffi::tiledb_subarray_add_range_by_name(
                                    ctx,
                                    c_subarray,
                                    c_name.as_ptr(),
                                    start.as_ptr() as *const std::ffi::c_void,
                                    end.as_ptr() as *const std::ffi::c_void,
                                    std::ptr::null(),
                                )
                            })?;
                        }
                    }
                })
            }
            Range::Multi(_) => unreachable!(
                "This is rejected by range.check_dimension_compatibility"
            ),
            Range::Var(range) => {
                var_value_range_go!(range, _DT, start, end, {
                    match key.into() {
                        LookupKey::Index(idx) => {
                            self.query.base().capi_call(|ctx| unsafe {
                                ffi::tiledb_subarray_add_range_var(
                                    ctx,
                                    c_subarray,
                                    idx as u32,
                                    start.as_ptr() as *const std::ffi::c_void,
                                    start.len() as u64,
                                    end.as_ptr() as *const std::ffi::c_void,
                                    end.len() as u64,
                                )
                            })?;
                        }
                        LookupKey::Name(name) => {
                            let c_name = cstring!(name);
                            self.query.base().capi_call(|ctx| unsafe {
                                ffi::tiledb_subarray_add_range_var_by_name(
                                    ctx,
                                    c_subarray,
                                    c_name.as_ptr(),
                                    start.as_ptr() as *const std::ffi::c_void,
                                    start.len() as u64,
                                    end.as_ptr() as *const std::ffi::c_void,
                                    end.len() as u64,
                                )
                            })?;
                        }
                    }
                })
            }
        }

        Ok(self)
    }

    /// Add a list of point ranges to the query.
    pub fn add_point_ranges<Key: Into<LookupKey>, T: PhysicalType>(
        self,
        key: Key,
        points: &[T],
    ) -> TileDBResult<Self> {
        let schema = self.query.base().query.array.schema()?;
        let dim_idx = schema.domain()?.dimension_index(key)?;
        let dim = schema.domain()?.dimension(dim_idx)?;
        let dtype = dim.datatype()?;
        if dtype.is_compatible_type::<T>() {
            return Err(Error::Datatype(DatatypeErrorKind::TypeMismatch {
                user_type: std::any::type_name::<T>(),
                tiledb_type: dtype,
            }));
        }

        if points.is_empty() {
            return Err(Error::InvalidArgument(anyhow!(
                "No point ranges provided to set."
            )));
        }

        let ctx = self.query.base().context();
        let c_subarray = *self.raw;

        ctx.capi_call(|ctx| unsafe {
            ffi::tiledb_subarray_add_point_ranges(
                ctx,
                c_subarray,
                dim_idx as u32,
                points.as_ptr() as *const std::ffi::c_void,
                points.len() as u64,
            )
        })?;

        Ok(self)
    }

    /// Apply the subarray to the query, returning the query builder.
    pub fn finish_subarray(self) -> TileDBResult<Q> {
        let c_query = **self.query.base().cquery();
        let c_subarray = *self.raw;

        self.query.base().capi_call(|ctx| unsafe {
            ffi::tiledb_query_set_subarray_t(ctx, c_query, c_subarray)
        })?;
        Ok(self.query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::array::*;
    use crate::query::{Query, QueryBuilder, ReadBuilder};
    use crate::Datatype;

    #[test]
    fn test_dense_ranges() -> TileDBResult<()> {
        let ctx = Context::new().unwrap();
        let tmp_dir = TempDir::new().unwrap();
        test_ranges(&ctx, ArrayType::Dense, &tmp_dir)
    }

    #[test]
    fn test_sparse_ranges() -> TileDBResult<()> {
        let ctx = Context::new().unwrap();
        let tmp_dir = TempDir::new().unwrap();
        test_ranges(&ctx, ArrayType::Sparse, &tmp_dir)
    }

    fn test_ranges(
        ctx: &Context,
        atype: ArrayType,
        dir: &TempDir,
    ) -> TileDBResult<()> {
        let array_uri = create_array(ctx, atype, dir)?;
        let array = Array::open(ctx, array_uri, Mode::Read)?;
        let query = ReadBuilder::new(array)?
            .start_subarray()?
            .add_range("id", &[1, 2])?
            .add_range("id", &[4, 6])?
            .add_range("id", &[8, 10])?
            .finish_subarray()?
            .build();

        let subarray = query.subarray()?;
        let ranges = subarray.ranges()?;

        // There's only one dimension with ranges
        assert_eq!(ranges.len(), 1);

        // The single id dimension has three ranges.
        assert_eq!(ranges[0].len(), 3);

        let expect = vec![
            Range::from(&[1i32, 2]),
            Range::from(&[4i32, 6]),
            Range::from(&[8i32, 10]),
        ];
        assert_eq!(ranges[0], expect);

        Ok(())
    }

    /// Create a simple dense test array with a couple fragments to inspect.
    fn create_array(
        ctx: &Context,
        atype: ArrayType,
        dir: &TempDir,
    ) -> TileDBResult<String> {
        let name = if atype == ArrayType::Dense {
            "range_test_dense"
        } else {
            "range_test_sparse"
        };
        let array_dir = dir.path().join(name);
        let array_uri = String::from(array_dir.to_str().unwrap());

        let domain = {
            let rows = DimensionBuilder::new(
                ctx,
                "id",
                Datatype::Int32,
                ([1, 10], 4),
            )?
            .build();

            DomainBuilder::new(ctx)?.add_dimension(rows)?.build()
        };

        let attr = AttributeBuilder::new(ctx, "attr", Datatype::Int32)?.build();
        let schema = SchemaBuilder::new(ctx, atype, domain)?
            .add_attribute(attr)?
            .build()?;

        Array::create(ctx, &array_uri, schema)?;

        Ok(array_uri)
    }
}
