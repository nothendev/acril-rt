use acril_rt::prelude::*;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // make sure the runtime can spawn !Send tasks
    let local_set = tokio::task::LocalSet::new();
    let _guard = local_set.enter();

    let runtime = acril_rt::Runtime::new();

    local_set
        .run_until(async move {}).await
}
