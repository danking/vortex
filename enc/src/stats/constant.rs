use crate::array::ArrayEncoding;
use std::collections::HashMap;

use crate::array::constant::ConstantArray;
use crate::array::stats::{Stat, StatsCompute, StatsSet};
use crate::scalar::{BoolScalar, PScalar};
use crate::types::DType;

impl StatsCompute for ConstantArray {
    fn compute(&self, _stat: &Stat) -> StatsSet {
        let mut m = HashMap::from([
            (Stat::Max, dyn_clone::clone_box(self.value())),
            (Stat::Min, dyn_clone::clone_box(self.value())),
            (Stat::IsConstant, true.into()),
            (Stat::IsSorted, true.into()),
            (Stat::RunCount, 1.into()),
        ]);

        if self.dtype() == &DType::Bool {
            m.insert(
                Stat::TrueCount,
                Box::new(PScalar::U64(
                    self.len() as u64
                        * self
                            .value()
                            .as_any()
                            .downcast_ref::<BoolScalar>()
                            .unwrap()
                            .value() as u64,
                )),
            );
        }
        StatsSet::from(m)
    }
}