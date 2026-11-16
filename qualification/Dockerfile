FROM ubuntu:24.04
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
       python3 iproute2 iputils-ping iptables ca-certificates openssl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /qualification
ENTRYPOINT ["python3", "/qualification/runtime.py"]
