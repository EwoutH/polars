use crate::prelude::*;
use crate::use_string_cache;
use crate::utils::arrow::array::{Array, ArrayBuilder};
use crate::vector_hasher::IdBuildHasher;
use ahash::AHashMap;
use arrow::array::{LargeStringArray, LargeStringBuilder};
use hashbrown::HashMap;
use polars_arrow::builder::PrimitiveArrayBuilder;
use std::marker::PhantomData;

pub enum RevMappingBuilder {
    Global(HashMap<u32, u32, IdBuildHasher>, LargeStringBuilder, u128),
    Local(LargeStringBuilder),
}

impl RevMappingBuilder {
    fn insert(&mut self, idx: u32, value: &str) {
        use RevMappingBuilder::*;
        match self {
            Local(builder) => builder.append_value(value).unwrap(),
            Global(map, builder, _) => {
                builder.append_value(value).unwrap();
                let new_idx = builder.len() as u32 - 1;
                map.insert(idx, new_idx);
            }
        };
    }

    fn finish(self) -> RevMapping {
        use RevMappingBuilder::*;
        match self {
            Local(mut b) => RevMapping::Local(b.finish()),
            Global(map, mut b, uuid) => RevMapping::Global(map, b.finish(), uuid),
        }
    }
}

pub enum RevMapping {
    Global(HashMap<u32, u32, IdBuildHasher>, LargeStringArray, u128),
    Local(LargeStringArray),
}

#[allow(clippy::len_without_is_empty)]
impl RevMapping {
    pub fn len(&self) -> usize {
        match self {
            Self::Global(_, a, _) => a.len(),
            Self::Local(a) => a.len(),
        }
    }

    pub fn get(&self, idx: u32) -> &str {
        match self {
            Self::Global(map, a, _) => {
                let idx = *map.get(&idx).unwrap();
                a.value(idx as usize)
            }
            Self::Local(a) => a.value(idx as usize),
        }
    }
    /// Check if the categoricals are created under the same global string cache.
    pub fn same_src(&self, other: &Self) -> bool {
        match (self, other) {
            (RevMapping::Global(_, _, l), RevMapping::Global(_, _, r)) => *l == *r,
            _ => false,
        }
    }
}

pub struct CategoricalChunkedBuilder {
    array_builder: PrimitiveArrayBuilder<UInt32Type>,
    field: Field,
    reverse_mapping: RevMappingBuilder,
}

impl CategoricalChunkedBuilder {
    pub fn new(name: &str, capacity: usize) -> Self {
        let builder = LargeStringBuilder::new(capacity / 10);
        let reverse_mapping = if use_string_cache() {
            let uuid = crate::STRING_CACHE.lock_map().uuid;
            RevMappingBuilder::Global(
                HashMap::with_hasher(IdBuildHasher::default()),
                builder,
                uuid,
            )
        } else {
            RevMappingBuilder::Local(builder)
        };

        CategoricalChunkedBuilder {
            array_builder: PrimitiveArrayBuilder::<UInt32Type>::new(capacity),
            field: Field::new(name, DataType::Categorical),
            reverse_mapping,
        }
    }
}
impl CategoricalChunkedBuilder {
    /// Appends all the values in a single lock of the global string cache.
    pub fn from_iter<'a, I>(&mut self, i: I)
    where
        I: IntoIterator<Item = Option<&'a str>>,
    {
        if use_string_cache() {
            let mut cache = crate::STRING_CACHE.lock_map();

            for opt_s in i {
                match opt_s {
                    Some(s) => {
                        let idx = match cache.map.get(s) {
                            Some(idx) => *idx,
                            None => {
                                let idx = cache.map.len() as u32;
                                cache.map.insert(s.to_string(), idx);
                                idx
                            }
                        };
                        self.reverse_mapping.insert(idx, s);
                        self.array_builder.append_value(idx);
                    }
                    None => {
                        self.array_builder.append_null();
                    }
                }
            }
        } else {
            let mut mapping = AHashMap::new();
            for opt_s in i {
                match opt_s {
                    Some(s) => {
                        let idx = match mapping.get(s) {
                            Some(idx) => *idx,
                            None => {
                                let idx = mapping.len() as u32;
                                mapping.insert(s, idx);
                                idx
                            }
                        };
                        self.reverse_mapping.insert(idx, s);
                        self.array_builder.append_value(idx);
                    }
                    None => {
                        self.array_builder.append_null();
                    }
                }
            }

            if mapping.len() > u32::MAX as usize {
                panic!("not more than {} categories supported", u32::MAX)
            };
        }
    }

    pub fn finish(mut self) -> ChunkedArray<CategoricalType> {
        let arr = Arc::new(self.array_builder.finish());
        ChunkedArray {
            field: Arc::new(self.field),
            chunks: vec![arr],
            phantom: PhantomData,
            categorical_map: Some(Arc::new(self.reverse_mapping.finish())),
        }
    }
}