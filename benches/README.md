# Benchmarks

Benchmarking currently looks at different runtime configurations in various scenarios: 
- executing a single, simple future, in the `simple` benchmark
- executing multiple more complex futures (with no io component) in the `rt-cfg` benchmark
- executing a future with an io component in the form of a TCP connection in the `parking` benchmark 

Configurations that are benchmarked include:
- varying whether or not executor and io poller threads park for a timeout (if there is no work to be done)
- varying whether or not executor and io poller threads park at all, regardless of presence or absense of work
- varying how many executor and io poller threads are spawned for the runtime
- varying if executor and io poller threads are assigned a core affinity
- others TBD

Note: in order to run these benchmarks, may need to set open file limit to something greater than typical default via
```bash
ulimit -n 10000
```
Where caller should set some appropriate limit (10000 is just a recommendation). Callers may also need to set
other appropriate rlimits depending on system