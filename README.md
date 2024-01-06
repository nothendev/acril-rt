# acril-rt - a small single-threaded runtime for Acril

Features:
- Tiny - less than 500 lines of code
- Fast - runs on an efficient event loop powered by Tokio
- Safe - `#![forbid(unsafe_code)]` here and in Acril

## Examples

```rust
use acril_rt::prelude::*;

struct Pinger {
    count: u32,
}

struct Ping;
struct Pong(u32);

impl Service for Pinger {
    type Context = Context<Self>;
    type Error = ();
}

impl Handler<Ping> for Pinger {
    type Response = Pong;

    async fn call(&mut self, _ping: Ping, _cx: &mut Self::Context) -> Result<Pong, Self::Error> {
        self.count += 1;

        Ok(Pong(self.count))
    }
}

struct GetCount;

impl Handler<GetCount> for Pinger {
    type Response = u32;

    async fn call(&mut self, _get_count: GetCount, _cx: &mut Self::Context) -> Result<u32, Self::Error> {
        Ok(self.count)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // make sure the runtime can spawn !Send tasks
    let local_set = tokio::task::LocalSet::new();
    let _guard = local_set.enter();

    let runtime = Runtime::new();

    local_set.run_until(async move {
        let addr = runtime.spawn(Pinger { count: 0 }).await;

        for i in 0..100 {
            let pong = addr.send(Ping).await.unwrap();
            assert_eq!(pong.0, i + 1);
        }

        assert_eq!(addr.send(GetCount).await.unwrap(), 100);
    }).await
}
```
