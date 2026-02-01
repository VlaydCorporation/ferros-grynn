pub use ferros_grynn_macros::StructureAccumulator;
use crate::gql_status::FerrosGrynnResult;
use crate::values::AnyValue;

pub fn build_from_map<A>(map: impl IntoIterator<Item = (String, AnyValue)>) -> FerrosGrynnResult<A::Output>
    where A: AccumulatorBuilder + Default
{
    let mut acc = A::default();
    for (k, v) in map {
        acc.add(k.as_str(), v)?;
    }

    acc.build()
}

pub trait StructureAccumulator {
    type Output;

    fn add(&mut self, field: &str, value: AnyValue) -> FerrosGrynnResult<()>;
}

pub trait AccumulatorBuilder: StructureAccumulator {
    fn build(self) -> FerrosGrynnResult<Self::Output>;
}