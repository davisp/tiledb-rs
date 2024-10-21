pub mod field;
pub mod write;

#[cfg(any(test, feature = "proptest-strategies"))]
pub mod strategy;

use std::cmp::Ordering;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt::Debug;
use std::ops::Range;

use proptest::bits::{BitSetLike, VarBitSet};

use tiledb_common::datatype::physical::{BitsEq, BitsOrd};

pub use self::field::FieldData;

#[derive(Clone, Debug, PartialEq)]
pub struct Cells {
    fields: HashMap<String, FieldData>,
}

impl Cells {
    /// # Panics
    ///
    /// Panics if the fields do not all have the same number of cells.
    pub fn new(fields: HashMap<String, FieldData>) -> Self {
        let mut expect_len: Option<usize> = None;
        for (_, d) in fields.iter() {
            if let Some(expect_len) = expect_len {
                assert_eq!(d.len(), expect_len);
            } else {
                expect_len = Some(d.len())
            }
        }

        Cells { fields }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.values().next().unwrap().is_empty()
    }

    pub fn len(&self) -> usize {
        self.fields.values().next().unwrap().len()
    }

    pub fn fields(&self) -> &HashMap<String, FieldData> {
        &self.fields
    }

    /// Copies data from the argument.
    /// Overwrites data at common indices and extends `self` where necessary.
    pub fn copy_from(&mut self, cells: Self) {
        for (field, data) in cells.fields.into_iter() {
            match self.fields.entry(field) {
                Entry::Vacant(v) => {
                    v.insert(data);
                }
                Entry::Occupied(mut o) => {
                    let prev_write_data = o.get_mut();
                    typed_field_data_cmp!(
                        prev_write_data,
                        data,
                        _DT,
                        ref mut mine,
                        theirs,
                        {
                            if mine.len() <= theirs.len() {
                                *mine = theirs;
                            } else {
                                mine[0..theirs.len()]
                                    .clone_from_slice(theirs.as_slice());
                            }
                        },
                        unreachable!()
                    );
                }
            }
        }
    }

    /// Shortens the cells, keeping the first `len` records and dropping the rest.
    pub fn truncate(&mut self, len: usize) {
        for data in self.fields.values_mut() {
            data.truncate(len)
        }
    }

    /// Extends this cell data with the contents of another.
    ///
    /// # Panics
    ///
    /// Panics if the set of fields in `self` and `other` do not match.
    ///
    /// Panics if any field in `self` and `other` has a different type.
    pub fn extend(&mut self, other: Self) {
        let mut other = other;
        for (field, data) in self.fields.iter_mut() {
            let other_data = other.fields.remove(field).unwrap();
            data.extend(other_data);
        }
        assert_eq!(other.fields.len(), 0);
    }

    /// Returns a view over a slice of the cells,
    /// with a subset of the fields viewed as indicated by `keys`.
    /// This is useful for comparing a section of `self` to another `Cells` instance.
    pub fn view<'a>(
        &'a self,
        keys: &'a [String],
        slice: Range<usize>,
    ) -> CellsView<'a> {
        for k in keys.iter() {
            if !self.fields.contains_key(k) {
                panic!("Cannot construct view: key '{}' not found (fields are {:?})",
                    k, self.fields.keys())
            }
        }

        CellsView {
            cells: self,
            keys,
            slice,
        }
    }

    /// Returns a comparator for ordering indices into the cells.
    fn index_comparator<'a>(
        &'a self,
        keys: &'a [String],
    ) -> impl Fn(&usize, &usize) -> Ordering + 'a {
        move |l: &usize, r: &usize| -> Ordering {
            for key in keys.iter() {
                typed_field_data_go!(self.fields[key], ref data, {
                    match BitsOrd::bits_cmp(&data[*l], &data[*r]) {
                        Ordering::Less => return Ordering::Less,
                        Ordering::Greater => return Ordering::Greater,
                        Ordering::Equal => continue,
                    }
                })
            }
            Ordering::Equal
        }
    }

    /// Returns whether the cells are sorted according to `keys`. See `Self::sort`.
    pub fn is_sorted(&self, keys: &[String]) -> bool {
        let index_comparator = self.index_comparator(keys);
        for i in 1..self.len() {
            if index_comparator(&(i - 1), &i) == Ordering::Greater {
                return false;
            }
        }
        true
    }

    /// Sorts the cells using `keys`. If two elements are equal on the first item in `keys`,
    /// then they will be ordered using the second; and so on.
    /// May not preserve the order of elements which are equal for all fields in `keys`.
    pub fn sort(&mut self, keys: &[String]) {
        let mut idx = std::iter::repeat(())
            .take(self.len())
            .enumerate()
            .map(|(i, _)| i)
            .collect::<Vec<usize>>();

        let idx_comparator = self.index_comparator(keys);
        idx.sort_by(idx_comparator);

        for data in self.fields.values_mut() {
            typed_field_data_go!(data, ref mut data, {
                let mut unsorted = std::mem::replace(
                    data,
                    vec![Default::default(); data.len()],
                );
                for i in 0..unsorted.len() {
                    data[i] = std::mem::take(&mut unsorted[idx[i]]);
                }
            });
        }
    }

    /// Returns a copy of the cells, sorted as if by `self.sort()`.
    pub fn sorted(&self, keys: &[String]) -> Self {
        let mut sorted = self.clone();
        sorted.sort(keys);
        sorted
    }

    /// Returns the list of offsets beginning each group, i.e. run of contiguous values on `keys`.
    ///
    /// This is best used with sorted cells, but that is not required.
    /// For each pair of offsets in the output, all cells in that index range are equal;
    /// and the adjacent cells outside of the range are not equal.
    pub fn identify_groups(&self, keys: &[String]) -> Option<Vec<usize>> {
        if self.is_empty() {
            return None;
        }
        let mut groups = vec![0];
        let mut icmp = 0;
        for i in 1..self.len() {
            let distinct = keys.iter().any(|k| {
                let v = self.fields().get(k).unwrap();
                typed_field_data_go!(
                    v,
                    ref cells,
                    cells[i].bits_ne(&cells[icmp])
                )
            });
            if distinct {
                groups.push(i);
                icmp = i;
            }
        }
        groups.push(self.len());
        Some(groups)
    }

    /// Returns the number of distinct values grouped on `keys`
    pub fn count_distinct(&self, keys: &[String]) -> usize {
        if self.len() <= 1 {
            return self.len();
        }

        let key_cells = {
            let key_fields = self
                .fields
                .iter()
                .filter(|(k, _)| keys.contains(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect::<HashMap<_, _>>();
            Cells::new(key_fields).sorted(keys)
        };

        let mut icmp = 0;
        let mut count = 1;

        for i in 1..key_cells.len() {
            let distinct = keys.iter().any(|k| {
                let v = key_cells.fields().get(k).unwrap();
                typed_field_data_go!(
                    v,
                    ref cells,
                    cells[i].bits_ne(&cells[icmp])
                )
            });
            if distinct {
                icmp = i;
                count += 1;
            }
        }

        count
    }

    /// Returns a subset of the records using the bitmap to determine which are included
    pub fn filter(&self, set: &VarBitSet) -> Cells {
        Self::new(
            self.fields()
                .iter()
                .map(|(k, v)| (k.clone(), v.filter(set)))
                .collect::<HashMap<String, FieldData>>(),
        )
    }

    /// Returns a subset of `self` containing only cells which have distinct values in `keys`
    /// such that `self.dedup(keys).count_distinct(keys) == self.len()`.
    /// The order of cells in the input is preserved and the
    /// first cell for each value of `keys` is preserved in the output.
    pub fn dedup(&self, keys: &[String]) -> Cells {
        if self.is_empty() {
            return self.clone();
        }

        let mut idx = (0..self.len()).collect::<Vec<usize>>();

        let idx_comparator = self.index_comparator(keys);
        idx.sort_by(idx_comparator);

        let mut icmp = 0;
        let mut preserve = VarBitSet::new_bitset(idx.len());
        preserve.set(idx[0]);

        for i in 1..idx.len() {
            let distinct = keys.iter().any(|k| {
                let v = self.fields.get(k).unwrap();
                typed_field_data_go!(
                    v,
                    ref field_cells,
                    field_cells[idx[i]].bits_ne(&field_cells[idx[icmp]])
                )
            });
            if distinct {
                icmp = i;
                preserve.set(idx[i]);
            }
        }

        self.filter(&preserve)
    }

    /// Returns a copy of `self` with only the fields in `fields`,
    /// or `None` if not all the requested fields are present.
    pub fn projection(&self, fields: &[&str]) -> Option<Cells> {
        let projection = fields
            .iter()
            .map(|f| {
                self.fields
                    .get(*f)
                    .map(|data| (f.to_string(), data.clone()))
            })
            .collect::<Option<HashMap<String, FieldData>>>()?;
        Some(Cells::new(projection))
    }

    /// Adds an additional field to `self`. Returns `true` if successful,
    /// i.e. the field data is valid for the current set of cells
    /// and there is not already a field for the key.
    pub fn add_field(&mut self, key: &str, values: FieldData) -> bool {
        if self.len() != values.len() {
            return false;
        }

        if self.fields.contains_key(key) {
            false
        } else {
            self.fields.insert(key.to_owned(), values);
            true
        }
    }
}

impl BitsEq for Cells {
    fn bits_eq(&self, other: &Self) -> bool {
        for (key, mine) in self.fields().iter() {
            if let Some(theirs) = other.fields().get(key) {
                if !mine.bits_eq(theirs) {
                    return false;
                }
            } else {
                return false;
            }
        }
        self.fields().keys().len() == other.fields().keys().len()
    }
}

pub struct StructuredCells {
    dimensions: Vec<usize>,
    cells: Cells,
}

impl StructuredCells {
    pub fn new(dimensions: Vec<usize>, cells: Cells) -> Self {
        let expected_cells: usize = dimensions.iter().cloned().product();
        assert_eq!(expected_cells, cells.len(), "Dimensions: {:?}", dimensions);

        StructuredCells { dimensions, cells }
    }

    pub fn num_dimensions(&self) -> usize {
        self.dimensions.len()
    }

    /// Returns the span of dimension `d`
    pub fn dimension_len(&self, d: usize) -> usize {
        self.dimensions[d]
    }

    pub fn into_inner(self) -> Cells {
        self.cells
    }

    pub fn slice(&self, slices: Vec<Range<usize>>) -> Self {
        assert_eq!(slices.len(), self.dimensions.len()); // this is doable but unimportant

        struct NextIndex<'a> {
            dimensions: &'a [usize],
            ranges: &'a [Range<usize>],
            cursors: Option<Vec<usize>>,
        }

        impl<'a> NextIndex<'a> {
            fn new(
                dimensions: &'a [usize],
                ranges: &'a [Range<usize>],
            ) -> Self {
                for r in ranges {
                    if r.is_empty() {
                        return NextIndex {
                            dimensions,
                            ranges,
                            cursors: None,
                        };
                    }
                }

                NextIndex {
                    dimensions,
                    ranges,
                    cursors: Some(
                        ranges.iter().map(|r| r.start).collect::<Vec<usize>>(),
                    ),
                }
            }

            fn compute(&self) -> usize {
                let Some(cursors) = self.cursors.as_ref() else {
                    unreachable!()
                };
                let mut index = 0;
                let mut scale = 1;
                for i in 0..self.dimensions.len() {
                    let i = self.dimensions.len() - i - 1;
                    index += cursors[i] * scale;
                    scale *= self.dimensions[i];
                }
                index
            }

            fn advance(&mut self) {
                let Some(cursors) = self.cursors.as_mut() else {
                    return;
                };
                for d in 0..self.dimensions.len() {
                    let d = self.dimensions.len() - d - 1;
                    if cursors[d] + 1 < self.ranges[d].end {
                        cursors[d] += 1;
                        return;
                    } else {
                        cursors[d] = self.ranges[d].start;
                    }
                }

                // this means that we reset the final dimension
                self.cursors = None;
            }
        }

        impl Iterator for NextIndex<'_> {
            type Item = usize;
            fn next(&mut self) -> Option<Self::Item> {
                if self.cursors.is_some() {
                    let index = self.compute();
                    self.advance();
                    Some(index)
                } else {
                    None
                }
            }
        }

        let mut v = VarBitSet::new_bitset(self.cells.len());

        NextIndex::new(self.dimensions.as_slice(), slices.as_slice())
            .for_each(|idx| v.set(idx));

        StructuredCells {
            dimensions: self.dimensions.clone(),
            cells: self.cells.filter(&v),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CellsView<'a> {
    cells: &'a Cells,
    keys: &'a [String],
    slice: Range<usize>,
}

impl<'b> PartialEq<CellsView<'b>> for CellsView<'_> {
    fn eq(&self, other: &CellsView<'b>) -> bool {
        // must have same number of values
        if self.slice.len() != other.slice.len() {
            return false;
        }

        for key in self.keys.iter() {
            let Some(mine) = self.cells.fields.get(key) else {
                // validated on construction
                unreachable!()
            };
            let Some(theirs) = other.cells.fields.get(key) else {
                return false;
            };

            typed_field_data_cmp!(
                mine,
                theirs,
                _DT,
                ref mine,
                ref theirs,
                if mine[self.slice.clone()] != theirs[other.slice.clone()] {
                    return false;
                },
                return false
            );
        }

        self.keys.len() == other.keys.len()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::rc::Rc;

    use proptest::prelude::*;
    use tiledb_common::datatype::physical::BitsKeyAdapter;
    use tiledb_pod::array::schema::SchemaData;

    use super::*;
    use crate::strategy::{CellsParameters, CellsStrategySchema};

    fn do_cells_extend(dst: Cells, src: Cells) {
        let orig_dst = dst.clone();
        let orig_src = src.clone();

        let mut dst = dst;
        dst.extend(src);

        for (fname, data) in dst.fields().iter() {
            let orig_dst = orig_dst.fields().get(fname).unwrap();
            let orig_src = orig_src.fields().get(fname).unwrap();

            typed_field_data_go!(data, ref dst, {
                assert_eq!(
                    *orig_dst,
                    FieldData::from(dst[0..orig_dst.len()].to_vec())
                );
                assert_eq!(
                    *orig_src,
                    FieldData::from(dst[orig_dst.len()..dst.len()].to_vec())
                );
                assert_eq!(dst.len(), orig_dst.len() + orig_src.len());
            });
        }

        // all Cells involved should have same set of fields
        assert_eq!(orig_dst.fields.len(), dst.fields.len());
        assert_eq!(orig_src.fields.len(), dst.fields.len());
    }

    fn do_cells_sort(cells: Cells, keys: Vec<String>) {
        let cells_sorted = cells.sorted(keys.as_slice());
        assert!(cells_sorted.is_sorted(keys.as_slice()));

        assert_eq!(cells.fields().len(), cells_sorted.fields().len());

        if cells.is_sorted(keys.as_slice()) {
            // running the sort should not have changed anything
            assert_eq!(cells, cells_sorted);
        }

        /*
         * We want to verify that the contents of the records are the
         * same before and after the sort. We can precisely do that
         * with a hash join, though it's definitely tricky to turn
         * the columnar data into rows, or we can approximate it
         * by sorting and comparing each column, which is not fully
         * precise but way easier.
         */
        for (fname, data) in cells.fields().iter() {
            let Some(data_sorted) = cells_sorted.fields().get(fname) else {
                unreachable!()
            };

            let orig_sorted = {
                let mut orig = data.clone();
                orig.sort();
                orig
            };
            let sorted_sorted = {
                let mut sorted = data_sorted.clone();
                sorted.sort();
                sorted
            };
            assert_eq!(orig_sorted, sorted_sorted);
        }
    }

    fn do_cells_slice_1d(cells: Cells, slice: Range<usize>) {
        let cells = StructuredCells::new(vec![cells.len()], cells);
        let sliced = cells.slice(vec![slice.clone()]).into_inner();
        let cells = cells.into_inner();

        assert_eq!(cells.fields().len(), sliced.fields().len());

        for (key, value) in cells.fields().iter() {
            let Some(sliced) = sliced.fields().get(key) else {
                unreachable!()
            };
            assert_eq!(
                value.slice(slice.start, slice.end - slice.start),
                *sliced
            );
        }
    }

    fn do_cells_slice_2d(
        cells: Cells,
        d1: usize,
        d2: usize,
        s1: Range<usize>,
        s2: Range<usize>,
    ) {
        let mut cells = cells;
        cells.truncate(d1 * d2);

        let cells = StructuredCells::new(vec![d1, d2], cells);
        let sliced = cells.slice(vec![s1.clone(), s2.clone()]).into_inner();
        let cells = cells.into_inner();

        assert_eq!(cells.fields().len(), sliced.fields().len());

        for (key, value) in cells.fields.iter() {
            let Some(sliced) = sliced.fields().get(key) else {
                unreachable!()
            };

            assert_eq!(s1.len() * s2.len(), sliced.len());

            typed_field_data_cmp!(
                value,
                sliced,
                _DT,
                ref value_data,
                ref sliced_data,
                {
                    for r in s1.clone() {
                        let value_start = (r * d2) + s2.start;
                        let value_end = (r * d2) + s2.end;
                        let value_expect = &value_data[value_start..value_end];

                        let sliced_start = (r - s1.start) * s2.len();
                        let sliced_end = (r + 1 - s1.start) * s2.len();
                        let sliced_cmp = &sliced_data[sliced_start..sliced_end];

                        assert_eq!(value_expect, sliced_cmp);
                    }
                },
                unreachable!()
            );
        }
    }

    fn do_cells_slice_3d(
        cells: Cells,
        d1: usize,
        d2: usize,
        d3: usize,
        s1: Range<usize>,
        s2: Range<usize>,
        s3: Range<usize>,
    ) {
        let mut cells = cells;
        cells.truncate(d1 * d2 * d3);

        let cells = StructuredCells::new(vec![d1, d2, d3], cells);
        let sliced = cells
            .slice(vec![s1.clone(), s2.clone(), s3.clone()])
            .into_inner();
        let cells = cells.into_inner();

        assert_eq!(cells.fields().len(), sliced.fields().len());

        for (key, value) in cells.fields.iter() {
            let Some(sliced) = sliced.fields.get(key) else {
                unreachable!()
            };

            assert_eq!(s1.len() * s2.len() * s3.len(), sliced.len());

            typed_field_data_cmp!(
                value,
                sliced,
                _DT,
                ref value_data,
                ref sliced_data,
                {
                    for z in s1.clone() {
                        for y in s2.clone() {
                            let value_start =
                                (z * d2 * d3) + (y * d3) + s3.start;
                            let value_end = (z * d2 * d3) + (y * d3) + s3.end;
                            let value_expect =
                                &value_data[value_start..value_end];

                            let sliced_start =
                                ((z - s1.start) * s2.len() * s3.len())
                                    + ((y - s2.start) * s3.len());
                            let sliced_end =
                                ((z - s1.start) * s2.len() * s3.len())
                                    + ((y + 1 - s2.start) * s3.len());
                            let sliced_cmp =
                                &sliced_data[sliced_start..sliced_end];

                            assert_eq!(value_expect, sliced_cmp);
                        }
                    }
                },
                unreachable!()
            );
        }
    }

    /// Assert that the output of [Cells::identify_groups] produces
    /// correct output for the given `keys`.
    fn do_cells_identify_groups(cells: Cells, keys: &[String]) {
        let Some(actual) = cells.identify_groups(keys) else {
            assert!(cells.is_empty());
            return;
        };

        for w in actual.windows(2) {
            let (start, end) = (w[0], w[1]);
            assert!(start < end);
        }

        for w in actual.windows(2) {
            let (start, end) = (w[0], w[1]);
            for k in keys.iter() {
                let f = cells.fields().get(k).unwrap();
                typed_field_data_go!(f, ref field_cells, {
                    for i in start..end {
                        assert!(field_cells[start].bits_eq(&field_cells[i]));
                    }
                })
            }
            if end < cells.len() {
                let some_ne = keys.iter().any(|k| {
                    let f = cells.fields().get(k).unwrap();
                    typed_field_data_go!(f, ref field_cells, {
                        field_cells[start].bits_ne(&field_cells[end])
                    })
                });
                assert!(some_ne);
            }
        }

        assert_eq!(Some(cells.len()), actual.last().copied());
    }

    fn do_cells_count_distinct_1d(cells: Cells) {
        for (key, field_cells) in cells.fields().iter() {
            let expect_count =
                typed_field_data_go!(field_cells, ref field_cells, {
                    let mut c = field_cells.clone();
                    c.sort_by(|l, r| l.bits_cmp(r));
                    c.dedup_by(|l, r| l.bits_eq(r));
                    c.len()
                });

            let keys_for_distinct = vec![key.clone()];
            let actual_count =
                cells.count_distinct(keys_for_distinct.as_slice());

            assert_eq!(expect_count, actual_count);
        }
    }

    fn do_cells_count_distinct_2d(cells: Cells) {
        let keys = cells.fields().keys().collect::<Vec<_>>();

        for i in 0..keys.len() {
            for j in 0..keys.len() {
                let expect_count = {
                    typed_field_data_go!(
                        cells.fields().get(keys[i]).unwrap(),
                        ref ki_cells,
                        {
                            typed_field_data_go!(
                                cells.fields().get(keys[j]).unwrap(),
                                ref kj_cells,
                                {
                                    let mut unique = HashMap::new();

                                    for r in 0..ki_cells.len() {
                                        let values = match unique
                                            .entry(BitsKeyAdapter(&ki_cells[r]))
                                        {
                                            Entry::Vacant(v) => {
                                                v.insert(HashSet::new())
                                            }
                                            Entry::Occupied(o) => o.into_mut(),
                                        };
                                        values.insert(BitsKeyAdapter(
                                            &kj_cells[r],
                                        ));
                                    }

                                    unique.values().flatten().count()
                                }
                            )
                        }
                    )
                };

                let keys_for_distinct = vec![keys[i].clone(), keys[j].clone()];
                let actual_count =
                    cells.count_distinct(keys_for_distinct.as_slice());

                assert_eq!(expect_count, actual_count);
            }
        }
    }

    fn do_cells_dedup(cells: Cells, keys: Vec<String>) {
        let dedup = cells.dedup(keys.as_slice());
        assert_eq!(dedup.len(), dedup.count_distinct(keys.as_slice()));

        // invariant check
        for field in dedup.fields().values() {
            assert_eq!(dedup.len(), field.len());
        }

        if dedup.is_empty() {
            assert!(cells.is_empty());
            return;
        } else if dedup.len() == cells.len() {
            assert_eq!(cells, dedup);
            return;
        }

        // check that order within the original cells is preserved
        assert_eq!(cells.view(&keys, 0..1), dedup.view(&keys, 0..1));

        let mut in_cursor = 1;
        let mut out_cursor = 1;

        while in_cursor < cells.len() && out_cursor < dedup.len() {
            if cells.view(&keys, in_cursor..(in_cursor + 1))
                == dedup.view(&keys, out_cursor..(out_cursor + 1))
            {
                out_cursor += 1;
                in_cursor += 1;
            } else {
                in_cursor += 1;
            }
        }
        assert_eq!(dedup.len(), out_cursor);
    }

    fn do_cells_projection(cells: Cells, keys: Vec<String>) {
        let proj = cells
            .projection(&keys.iter().map(|s| s.as_ref()).collect::<Vec<&str>>())
            .unwrap();

        for key in keys.iter() {
            let Some(field_in) = cells.fields().get(key) else {
                unreachable!()
            };
            let Some(field_out) = proj.fields().get(key) else {
                unreachable!()
            };

            assert_eq!(field_in, field_out);
        }

        // everything in `keys` is in the projection, there should be no other fields
        assert_eq!(keys.len(), proj.fields().len());
    }

    proptest! {
        #[test]
        fn cells_extend((dst, src) in any::<SchemaData>().prop_flat_map(|s| {
            let params = CellsParameters {
                schema: Some(CellsStrategySchema::WriteSchema(Rc::new(s))),
                ..Default::default()
            };
            (any_with::<Cells>(params.clone()), any_with::<Cells>(params.clone()))
        })) {
            do_cells_extend(dst, src)
        }

        #[test]
        fn cells_sort((cells, keys) in any::<Cells>().prop_flat_map(|c| {
            let keys = c.fields().keys().cloned().collect::<Vec<String>>();
            let nkeys = keys.len();
            (Just(c), proptest::sample::subsequence(keys, 0..=nkeys).prop_shuffle())
        })) {
            do_cells_sort(cells, keys)
        }

        #[test]
        fn cells_slice_1d((cells, bound1, bound2) in any::<Cells>().prop_flat_map(|cells| {
            let slice_min = 0;
            let slice_max = cells.len();
            (Just(cells),
            slice_min..=slice_max,
            slice_min..=slice_max)
        })) {
            let start = std::cmp::min(bound1, bound2);
            let end = std::cmp::max(bound1, bound2);
            do_cells_slice_1d(cells, start.. end)
        }

        #[test]
        fn cells_slice_2d((cells, d1, d2, b11, b12, b21, b22) in any_with::<Cells>(CellsParameters {
            min_records: 1,
            ..Default::default()
        }).prop_flat_map(|cells| {
            let ncells = cells.len();
            (Just(cells),
            1..=((ncells as f64).sqrt() as usize),
            1..=((ncells as f64).sqrt() as usize))
                .prop_flat_map(|(cells, d1, d2)| {
                    (Just(cells),
                    Just(d1),
                    Just(d2),
                    0..=d1,
                    0..=d1,
                    0..=d2,
                    0..=d2)
                })
        })) {
            let s1 = std::cmp::min(b11, b12).. std::cmp::max(b11, b12);
            let s2 = std::cmp::min(b21, b22).. std::cmp::max(b21, b22);
            do_cells_slice_2d(cells, d1, d2, s1, s2)
        }

        #[test]
        fn cells_slice_3d((cells, d1, d2, d3, b11, b12, b21, b22, b31, b32) in any_with::<Cells>(CellsParameters {
            min_records: 1,
            ..Default::default()
        }).prop_flat_map(|cells| {
            let ncells = cells.len();
            (Just(cells),
            1..=((ncells as f64).cbrt() as usize),
            1..=((ncells as f64).cbrt() as usize),
            1..=((ncells as f64).cbrt() as usize))
                .prop_flat_map(|(cells, d1, d2, d3)| {
                    (Just(cells),
                    Just(d1),
                    Just(d2),
                    Just(d3),
                    0..=d1,
                    0..=d1,
                    0..=d2,
                    0..=d2,
                    0..=d3,
                    0..=d3)
                })
        })) {
            let s1 = std::cmp::min(b11, b12).. std::cmp::max(b11, b12);
            let s2 = std::cmp::min(b21, b22).. std::cmp::max(b21, b22);
            let s3 = std::cmp::min(b31, b32).. std::cmp::max(b31, b32);
            do_cells_slice_3d(cells, d1, d2, d3, s1, s2, s3)
        }

        #[test]
        fn cells_identify_groups((cells, keys) in any::<Cells>().prop_flat_map(|c| {
            let keys = c.fields().keys().cloned().collect::<Vec<String>>();
            let nkeys = keys.len();
            (Just(c), proptest::sample::subsequence(keys, 0..=nkeys))
        }))
        {
            do_cells_identify_groups(cells, &keys)
        }

        #[test]
        fn cells_count_distinct_1d(cells in any::<Cells>()) {
            do_cells_count_distinct_1d(cells)
        }

        #[test]
        fn cells_count_distinct_2d(cells in any::<Cells>()) {
            prop_assume!(cells.fields().len() >= 2);
            do_cells_count_distinct_2d(cells)
        }

        #[test]
        fn cells_dedup((cells, keys) in any::<Cells>().prop_flat_map(|c| {
            let keys = c.fields().keys().cloned().collect::<Vec<String>>();
            let nkeys = keys.len();
            (Just(c), proptest::sample::subsequence(keys, 0..=nkeys).prop_shuffle())
        }))
        {
            do_cells_dedup(cells, keys)
        }

        #[test]
        fn cells_projection((cells, keys) in any::<Cells>().prop_flat_map(|c| {
            let keys = c.fields().keys().cloned().collect::<Vec<String>>();
            let nkeys = keys.len();
            (Just(c), proptest::sample::subsequence(keys, 0..=nkeys).prop_shuffle())
        })) {
            do_cells_projection(cells, keys)
        }
    }
}
