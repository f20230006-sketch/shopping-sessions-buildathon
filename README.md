# Shopping Sessions

A native Rust implementation of the [Databricks Buildathon track](https://www.joinbuilders.org/buildathon/tracks/shopping-sessions).

The pipeline memory-maps the CSV parts and divides them into line-aligned chunks. It resolves linked users by union–find, partitions parsed events by event ID to retain the earliest copy, then partitions events and clicks by canonical user. Each partition sorts the two streams and evaluates sessions and rolling seven-day campaign membership. It writes typed Parquet directly.

~~~sh
docker build --platform linux/amd64 -t shopping-sessions .
docker run --rm --network none --read-only --cpus 8 --memory 24g \
  --user 1000:1000 -e NCPU=8 \
  -v "$PWD/data:/data:ro" -v "$PWD/output:/output" shopping-sessions
~~~

Use writable output storage for uid 1000. By default the binary reads `/data` and writes `/output`. It also accepts `DATA_DIR`, `OUTPUT_DIR`, or two positional paths for local testing. `NCPU` sets the thread count.

The official starter checks passed all 36 fixtures and matched both 1M-event dev tables exactly. The native executable processed a separately generated 20M-event and 5M-click dataset in 3.83 seconds on the local test machine with eight threads. This measurement excludes container startup and uses public synthetic data.
