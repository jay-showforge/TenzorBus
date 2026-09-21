# TenzorBus benchmarks

The final `v0.1.0-alpha.0` gate completed **49/49 cases with zero benchmark
errors** on an **AMD EPYC 9V74, 9-vCPU Linux KVM** host.

TenzorBus does not win every single-consumer latency or throughput metric. Its
primary design target is shared-memory fan-out and copy scaling: one publication
and one producer-side copy for an existing payload, with zero-copy consumer
views as consumer count grows.

The benchmark isolates local transport. Media decode and model inference are
excluded. Each consumer creates a NumPy view and performs the same fixed-stride
read. With multiple consumers, point-to-point transports deliver once per
consumer while TenzorBus publishes once for all consumers; this fan-out cost is
part of the behavior being measured. Connection setup is outside the timed
region.

The VM's seccomp policy blocked pathname Unix sockets at
`socket(AF_UNIX, ...)`. Unix rows therefore use real `AF_UNIX/SOCK_STREAM`
socketpairs inherited by independent consumer processes across `exec`, one pair
per consumer. No Unix cases were skipped.

> **Comparison caveat:** `tenzorbus_direct_nofill` measures the transport floor
> for in-place generation. It excludes generation/fill and is not directly
> comparable to paths that copy an existing payload.

| Payload | Consumers | Path | Median ms | p95 ms | p99 ms | Pub/s | Copies |
|---|---:|---|---:|---:|---:|---:|---:|
| 64 KiB | 1 | tenzorbus_copy | 0.0206 | 0.0539 | 0.1699 | 22,283.9 | 1 |
| 64 KiB | 1 | tenzorbus_direct | 0.0070 | 0.0245 | 1.7723 | 22,206.1 | 1 |
| 64 KiB | 1 | tenzorbus_direct_nofill | 0.0027 | 0.0825 | 0.1654 | 24,324.2 | 0 |
| 64 KiB | 1 | unix_socket | 0.0164 | 0.0791 | 2.6040 | 37,765.4 | 2 |
| 64 KiB | 1 | fifo | 0.0583 | 0.1242 | 0.2600 | 16,560.0 | 2 |
| 64 KiB | 1 | http_binary | 0.0502 | 0.1086 | 0.2007 | 8,385.5 | 3 |
| 64 KiB | 1 | http_json_b64 | 0.3150 | 0.4126 | 0.6241 | 1,420.1 | 6 |
| 588 KiB | 1 | tenzorbus_copy | 0.0544 | 0.0802 | 3.6084 | 15,006.5 | 1 |
| 588 KiB | 1 | tenzorbus_direct | 0.0324 | 1.0338 | 5.5121 | 8,124.2 | 1 |
| 588 KiB | 1 | tenzorbus_direct_nofill | 0.0129 | 0.2481 | 2.4487 | 15,378.0 | 0 |
| 588 KiB | 1 | unix_socket | 0.1308 | 0.3883 | 4.5037 | 3,367.3 | 2 |
| 588 KiB | 1 | fifo | 0.2546 | 0.4865 | 1.0815 | 3,030.7 | 2 |
| 588 KiB | 1 | http_binary | 0.1150 | 0.3803 | 0.5755 | 3,764.4 | 3 |
| 588 KiB | 1 | http_json_b64 | 3.5306 | 9.1518 | 10.6166 | 137.5 | 6 |
| 4 MiB | 1 | tenzorbus_copy | 0.6023 | 5.6162 | 6.0215 | 921.6 | 1 |
| 4 MiB | 1 | tenzorbus_direct | 0.2444 | 5.5885 | 6.7418 | 1,480.7 | 1 |
| 4 MiB | 1 | tenzorbus_direct_nofill | 0.0165 | 2.2863 | 2.5760 | 1,617.1 | 0 |
| 4 MiB | 1 | unix_socket | 0.6948 | 6.5217 | 12.2449 | 664.9 | 2 |
| 4 MiB | 1 | fifo | 2.0698 | 8.9832 | 10.1887 | 330.7 | 2 |
| 4 MiB | 1 | http_binary | 2.1372 | 3.1344 | 4.7329 | 456.6 | 3 |
| 4 MiB | 1 | http_json_b64 | 23.1506 | 40.1447 | 40.1447 | 23.0 | 6 |
| 16 MiB | 1 | tenzorbus_copy | 1.6764 | 8.6112 | 13.4328 | 384.3 | 1 |
| 16 MiB | 1 | tenzorbus_direct | 2.2188 | 9.2045 | 17.3930 | 286.3 | 1 |
| 16 MiB | 1 | tenzorbus_direct_nofill | 0.0158 | 1.5681 | 5.3836 | 318.2 | 0 |
| 16 MiB | 1 | unix_socket | 5.7339 | 11.8859 | 17.8809 | 157.3 | 2 |
| 16 MiB | 1 | fifo | 11.6620 | 28.3229 | 38.3837 | 75.0 | 2 |
| 16 MiB | 1 | http_binary | 4.6157 | 13.5294 | 20.9581 | 166.9 | 3 |
| 16 MiB | 1 | http_json_b64 | 108.7102 | 119.9385 | 119.9385 | 5.3 | 6 |
| 588 KiB | 2 | tenzorbus_copy | 0.2767 | 0.3881 | 0.4812 | 3,620.6 | 1 |
| 588 KiB | 2 | tenzorbus_direct | 0.2545 | 0.4664 | 3.0824 | 2,744.5 | 1 |
| 588 KiB | 2 | tenzorbus_direct_nofill | 0.0165 | 0.0510 | 0.2834 | 7,526.5 | 0 |
| 588 KiB | 2 | unix_socket | 0.0809 | 0.1670 | 0.3389 | 4,932.4 | 3 |
| 588 KiB | 2 | fifo | 0.3464 | 1.6839 | 1.9781 | 1,131.7 | 3 |
| 588 KiB | 2 | http_binary | 0.1255 | 0.3663 | 0.5544 | 2,143.6 | 5 |
| 588 KiB | 2 | http_json_b64 | 2.7010 | 3.8725 | 15.3251 | 83.3 | 9 |
| 588 KiB | 4 | tenzorbus_copy | 0.1186 | 0.3382 | 0.3867 | 6,637.4 | 1 |
| 588 KiB | 4 | tenzorbus_direct | 0.0585 | 0.1293 | 0.2453 | 10,360.6 | 1 |
| 588 KiB | 4 | tenzorbus_direct_nofill | 0.0256 | 3.4016 | 30.0467 | 3,246.3 | 0 |
| 588 KiB | 4 | unix_socket | 0.1822 | 0.4940 | 1.5056 | 1,135.6 | 5 |
| 588 KiB | 4 | fifo | 0.3275 | 0.5982 | 0.9751 | 695.7 | 5 |
| 588 KiB | 4 | http_binary | 0.1459 | 0.3634 | 0.6205 | 984.7 | 9 |
| 588 KiB | 4 | http_json_b64 | 2.6590 | 4.3261 | 71.0354 | 35.9 | 15 |
| 588 KiB | 8 | tenzorbus_copy | 0.3496 | 0.5517 | 0.7028 | 2,834.9 | 1 |
| 588 KiB | 8 | tenzorbus_direct | 0.3003 | 0.4801 | 1.6344 | 2,350.1 | 1 |
| 588 KiB | 8 | tenzorbus_direct_nofill | 0.0359 | 0.3424 | 1.1393 | 4,930.4 | 0 |
| 588 KiB | 8 | unix_socket | 0.1050 | 0.2006 | 0.3965 | 1,121.3 | 9 |
| 588 KiB | 8 | fifo | 0.3043 | 0.4835 | 1.5608 | 381.4 | 9 |
| 588 KiB | 8 | http_binary | 0.1625 | 0.4097 | 0.6423 | 451.3 | 17 |
| 588 KiB | 8 | http_json_b64 | 2.6356 | 4.4261 | 9.2295 | 21.3 | 27 |

All rows completed full delivery. The accepted source raw JSON has SHA-256
`ba86271180b80da10a1c542312b14b2a8c643713eefb728c2aa12db45771debe`.
See `evidence/epyc-benchmark/README.md` for provenance and evidence-handling
details.
