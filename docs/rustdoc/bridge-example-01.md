```
use ferros_grynn::{
	bridge::{
		Bridge,
		BridgeContext,
		BridgeData,
		BridgeFuture,
		BridgeOperation,
		FerrosQueryBridgeExt,
	},
	datastore::Datastore,
	gql_status::FerrosGrynnResult,
	values::Value,
};
struct Source;
impl Bridge for Source {
	fn execute(&self, _: BridgeOperation, context: BridgeContext) -> BridgeFuture<'_> {
		Box::pin(async move {
			context.check()?;
			Ok(Value::Null)
		})
	}
}
# fn main() -> FerrosGrynnResult<()> {
let runtime =
	tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| {
		ferros_grynn::gql_status::FerrosGrynnError::bridge_operation_error(
			"example runtime failed",
		)
		.with_technical_source(error)
	})?;
let store = Datastore::in_memory();
let query = FerrosQueryBridgeExt::use_bridge(
	store.query("RETURN $bridge"),
	Source,
	BridgeData::ExternalQuery("native source", None),
);
let result = runtime.block_on(query.execute())?;
assert_eq!(result.results[0].result.clone()?.into_parts().0, Value::Null);
# Ok(()) }
```
