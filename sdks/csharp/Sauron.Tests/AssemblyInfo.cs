using Xunit;

// Several tests exercise the process-global Scope (ScopeManager.Global). Running test
// collections in parallel would let one test mutate the global scope while another reads
// it during capture (concurrent Dictionary read+write). The suite is small and fast, so
// serialize it for deterministic, race-free runs. The genuine async-local isolation is
// still verified inside ScopeTests.ConcurrentScopes_DoNotLeak via Task.Run.
[assembly: CollectionBehavior(DisableTestParallelization = true)]
