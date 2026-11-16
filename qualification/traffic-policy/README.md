# Traffic policy candidate

The isolated engine assigns a class to locally originated IP traffic before
compression or encryption. DSCP CS5 is control; CS4 and ICMP are telemetry;
UDP source/destination ports 8000 and 8001 identify video. Unknown or fragmented
traffic is bulk. Classification uses packet markings; it does not authorize the application.

The Noise handshake binds `avian-peerconn-noise-hop-policy-v1`; peers using the
earlier authentication-only domain are rejected before packet forwarding.
The inner header carries the class inside the authenticated hop envelope. Relays
preserve it without reading encrypted payloads. Every selected physical connection
checks its actual local socket address against the startup policy. TCP listeners
record the accepted socket address rather than their wildcard listener address.
Unknown socket addresses fail closed when metered links are configured.

Example startup environment:

```sh
export AVIAN_TRAFFIC_POLICY='{"metered_addresses":["192.0.2.10"],"allow_metered_video":false,"allow_metered_bulk":false}'
```

The actual local addresses must be supplied by the platform adapter; absent policy
means no links have been declared metered. Malformed policy rejects application
data. Defaults allow control and telemetry over declared metered links, while
video and bulk require explicit allowances. Enrolled relays remain trusted policy
enforcers; this does not prevent a compromised member from changing a class.

Build with membership, authenticated-forwarding, and traffic-policy patches in
that order, then run:

```sh
python3 qualification/traffic-policy/run.py --engine-directory /path/to/binaries \
  --output qualification/results/traffic-policy
```

The isolated Linux harness sends real UDP through a relay and over a direct link.
Denial checks bracket each attempted transfer with permitted control traffic on
the same port. Tests include both directions and an explicit video allowance.

The engine selects an eligible physical connection to an already selected neighbor,
preferring measured latency. It does not advertise metered edges throughout the
topology. A blocked next hop can prevent delivery despite another topology route.
Policy is loaded at startup. Relays remain trusted policy enforcers.
