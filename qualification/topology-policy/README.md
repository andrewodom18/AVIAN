# Topology policy

Build and run the isolated candidate:

```sh
python3 qualification/build.py --topology-policy --output .local/topology-engine
python3 qualification/topology-policy/run.py --engine-directory .local/topology-engine \
  --output qualification/results/topology-policy
```

Each node advertises directed physical connections, allowed traffic classes, and
measured latency through the authenticated routing exchange. Parallel connections
retain separate class permissions and costs. Routes are calculated per class on
topology changes. Direct destinations use the same policy lookup as relay paths.
Latencies are quantized to milliseconds; unknown latency stays zero in the
advertisement and receives a conservative routing penalty. Capacity is unmeasured;
path ranking uses latency among permitted paths.

Advertisements refresh every five seconds. Their freshness expires twenty seconds
after local monotonic receipt of a newer version. Repeated copies of the same
version cannot renew it. Expired policy records cannot supply forwarding routes.
The handshake domain is `avian-peerconn-noise-hop-topology-v1`; earlier variants
are incompatible. Physical egress enforcement remains active on every hop.

`AVIAN_TRAFFIC_POLICY` accepts `control_tcp_ports`, an explicit list of nonzero
TCP ports to classify as control in both directions. For example:

```sh
export AVIAN_TRAFFIC_POLICY='{"metered_addresses":["192.0.2.10"],"control_tcp_ports":[4180]}'
```

The four-node test provides a shorter metered path and a longer permitted path,
then removes and restores the permitted relay. It tests video in both directions,
control through the remaining metered path, prohibited bulk/video, and explicit
remote video allowance. Private test keys stay in the disposable container.
Results apply to the tested topology and load; they do not establish bandwidth
optimization, flow pinning, congestion scheduling, or hardware support.
