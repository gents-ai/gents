# engineer_plugin_authoring

Native eval suite exercising zero-bash JavaScript WASM plugin authoring (`PluginSource`, sealed compilation via `javy`, inline test cases, `data_calls` reads, atomic `writes`, in-place rebuild with digest pin migration, specialist agent delegation, and `pack export` + `pack build` bundling compiled plugins with schemas and pages).

Run through `scripts/evals/run-container.sh` with `command_execution=off` and the `javy 8.1.1`-equipped eval image.
