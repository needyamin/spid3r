# Freestanding Profile (`--freestanding`) — Systems Track Only

`--freestanding` builds an AOT binary against the minimal runtime entry
(`spid3r_rt_start_freestanding`), omitting scheduler, network, timezone, and full
stdlib initialization, and supports custom entry symbol naming via `--entry <sym>`
plus object emission via `--emit-obj`.

> **Note:** `--freestanding` is intended for systems-track / OS & bare-metal
> experiments; normal applications should use standard `spid3r build`.

```sh
spid3r build --freestanding examples/freestanding/hello.matrix -o hello_fs
./hello_fs
```
