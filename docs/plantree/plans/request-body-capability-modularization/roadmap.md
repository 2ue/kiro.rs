# Roadmap

## Done

- Inventory current request body processing surfaces from code.
- Register plan root and baseline links.
- Added explicit request/body capability plan types for parsed Anthropic, local Kiro, and external body pipelines.
- Routed parsed Anthropic preprocessing, local Kiro body preparation, and external raw/normalized body preparation through those plans with compatibility defaults.
- Split `converter.rs` internals into schema, model, content, tools, tool-pairing, and history modules.
- Added `BodyConversionConfig` and wired it through runtime config, request config, local body planning, and both React admin surfaces.
- Extended fake upstream loadtest scenarios with random, dense, tiered 3/10/22 second slow first byte, and mixed chaos.
- Validated raw and normalized external pool paths against fake upstream normal, slow, long-context, high-concurrency, error, recovery, and mixed-chaos scenarios.
- Verified raw body passthrough with optional model rewrite and with explicit direct disabled.
- Verified usage projection and external-pool billing remain independent from raw/normalized body mode.

## In Progress

- Final cleanup of the temporary validation proxy, database, and Redis namespace.

## Next

- Consider a route planner extraction that can choose non-raw external targets before parsed-body preprocessing when enough raw facts are available.
- Keep profiling the normalized long-context path if CPU/RSS pressure reappears under real upstream slow streams.

## Deferred

- Full plugin/trait system for every processing stage.
- Route planner that can select target before all parsed-body processing for more non-raw external cases.
