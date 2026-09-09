import assert from "node:assert/strict";
import test from "node:test";
import {
  AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
  applyEnvironmentDefaultsToNodes,
  assignRadioMix,
  calculatePacketTrafficMbps,
  calculateMimo,
  calculateNetwork,
  createNetworkNodes,
  DEFAULT_MIMO_INPUTS,
  DEFAULT_NETWORK_INPUTS,
  RADIO_PROFILES,
} from "../app/model.ts";

test("converts 3 kB messages into per-node traffic with planning overhead", () => {
  const typicalTraffic = calculatePacketTrafficMbps(3, 1, 20);
  assert.equal(typicalTraffic, 0.0288);
  assert.equal(DEFAULT_NETWORK_INPUTS.trafficPerNodeMbps, typicalTraffic);
  assert.equal(typicalTraffic + 5.5, 5.5288);
});

test("reproduces the supplied workbook default outputs", () => {
  const result = calculateMimo(DEFAULT_MIMO_INPUTS);
  const expected = [
    [20, 2, 10, 29.47, -86.7503080855, 9.2496919145],
    [20, 1, 3, 19.65, -86.7503080855, 9.2496919145],
    [10, 2, 11, 19.9, -87.7503080855, 11.2496919145],
    [10, 1, 4, 14.8, -87.7503080855, 11.2496919145],
    [5, 2, 12, 12.38, -87.7503080855, 14.2496919145],
    [5, 1, 4, 6.18, -87.7503080855, 14.2496919145],
    [2.5, 2, 12, 6.6, -89.7503080855, 15.2496919145],
    [2.5, 1, 5, 4.4, -89.7503080855, 15.2496919145],
    [1.25, 2, 13, 4.35, -89.7503080855, 18.2496919145],
    [1.25, 1, 6, 2.47, -89.7503080855, 18.2496919145],
  ];
  assert.equal(result.overallStatus, "Reliable");
  assert.ok(Math.abs(result.horizonKm - 81.1624039679) < 1e-9);
  assert.ok(Math.abs(result.fresnelRadiusMeters - 13.832768588) < 1e-9);
  for (const [index, [bandwidth, nss, mcs, capacity, rssi, snr]] of expected.entries()) {
    const mode = result.modes[index];
    assert.equal(mode.bandwidth, bandwidth);
    assert.equal(mode.nss, nss);
    assert.equal(mode.mcs, mcs);
    assert.equal(mode.capacity, capacity);
    assert.ok(Math.abs(mode.rssi - rssi) < 0.000001);
    assert.ok(Math.abs(mode.snr - snr) < 0.000001);
  }
});

test("uses the workbook PA/BDA adaptive backoff curve", () => {
  const native = calculateMimo({ ...DEFAULT_MIMO_INPUTS, targetDistanceKm: 0.1, bdaUsed: false, paGain: 0 });
  const amplified = calculateMimo({ ...DEFAULT_MIMO_INPUTS, targetDistanceKm: 0.1, bdaUsed: true, paGain: 0 });
  const native20 = native.modes.find((mode) => mode.bandwidth === 20 && mode.nss === 2);
  const amplified20 = amplified.modes.find((mode) => mode.bandwidth === 20 && mode.nss === 2);
  assert.equal(native20.mcs, 15);
  assert.equal(amplified20.mcs, 15);
  assert.ok(Math.abs((native20.rssi - amplified20.rssi) - 4) < 1e-9);
});

test("applies the same receive-path adjustment to range and displayed signal", () => {
  const fourPath = calculateMimo({ ...DEFAULT_MIMO_INPUTS, targetDistanceKm: 0.1, rxAntennas: 4 });
  const twoPath = calculateMimo({ ...DEFAULT_MIMO_INPUTS, targetDistanceKm: 0.1, rxAntennas: 2 });
  for (const fourPathMode of fourPath.modes) {
    const twoPathMode = twoPath.modes.find(
      (mode) => mode.bandwidth === fourPathMode.bandwidth && mode.nss === fourPathMode.nss,
    );
    assert.ok(twoPathMode);
    assert.ok(Math.abs(fourPathMode.rssi - twoPathMode.rssi) < 1e-9);
    assert.ok(Math.abs((fourPathMode.snr - twoPathMode.snr) - 3) < 1e-9);
  }
});

test("supports the full 150-node network ceiling", () => {
  const nodes = createNetworkNodes(150);
  const result = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, topology: "Direct to hub" });
  assert.equal(nodes.length, 150);
  assert.equal(result.nodeCount, 150);
  assert.equal(result.links.length, 149);
});

test("accounts for aggregate half-duplex airtime at gateways and relays", () => {
  const direct = calculateNetwork(createNetworkNodes(150), {
    ...DEFAULT_NETWORK_INPUTS,
    averageDistanceKm: 1,
    trafficPerNodeMbps: 5.5288,
  });
  assert.equal(direct.invalidLinks, 0);
  assert.equal(direct.mostLoadedNodeId, 1);
  assert.ok(direct.gatewayDutyCyclePercent > 100);
  assert.equal(direct.status, "Invalid");

  const chain = calculateNetwork(createNetworkNodes(6), {
    ...DEFAULT_NETWORK_INPUTS,
    topology: "Relay chain",
    averageDistanceKm: 1,
    trafficPerNodeMbps: 5,
  });
  assert.ok(chain.maxNodeDutyCyclePercent >= chain.gatewayDutyCyclePercent);
});

test("matches workbook cross-polarization rules in network mode", () => {
  const nodes = createNetworkNodes(2);
  const noCrossPol = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, averageDistanceKm: 1, crossPolarization: "No" });
  const oneSide = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, averageDistanceKm: 1, crossPolarization: "One Side" });
  const bothSides = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, averageDistanceKm: 1, crossPolarization: "Both Sides" });
  assert.equal(noCrossPol.links[0].spatialStreams, 1);
  assert.equal(oneSide.links[0].spatialStreams, 1);
  assert.equal(bothSides.links[0].spatialStreams, 2);
  assert.ok(Math.abs((noCrossPol.links[0].snrDb - oneSide.links[0].snrDb) - 3) < 1e-9);
  assert.ok(bothSides.links[0].capacityMbps > oneSide.links[0].capacityMbps);
});

test("accumulates downstream traffic on relay-chain links", () => {
  const nodes = createNetworkNodes(6);
  const result = calculateNetwork(nodes, {
    ...DEFAULT_NETWORK_INPUTS,
    topology: "Relay chain",
    trafficPerNodeMbps: 2,
  });
  assert.equal(result.links[0].requiredMbps, 10);
  assert.equal(result.links.at(-1).requiredMbps, 2);
});

test("constrains the estimated SL5200 profile to documented frequency ranges", () => {
  const nodes = createNetworkNodes(2, "sl5200");
  const supported = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, averageDistanceKm: 1, frequencyMHz: 2350 });
  const unsupported = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, averageDistanceKm: 1, frequencyMHz: 5800 });
  assert.equal(RADIO_PROFILES.sl5200.maxTxPowerW, 2);
  assert.equal(RADIO_PROFILES.sl5200.receivePaths, 2);
  assert.equal(RADIO_PROFILES.sl5200.receiverSensitivityOffsetDb, -2);
  assert.notEqual(supported.links[0].capacityMbps, null);
  assert.equal(unsupported.links[0].status, "Invalid");
  assert.equal(unsupported.links[0].capacityMbps, null);
});

test("invalidates an out-of-band SL5200 single-link scenario", () => {
  const result = calculateMimo({ ...DEFAULT_MIMO_INPUTS, radioProfile: "sl5200", frequencyMHz: 5800, txPowerW: 2, rxAntennas: 2 });
  assert.equal(result.frequencySupported, false);
  assert.equal(result.overallStatus, "No viable mode");
});

test("calculates multi-node environment, path-loss, and Fresnel summaries", () => {
  const result = calculateNetwork(createNetworkNodes(8), DEFAULT_NETWORK_INPUTS);
  assert.equal(result.environment.exponent, 2.3);
  assert.ok(result.averageLinkDistanceKm >= DEFAULT_NETWORK_INPUTS.averageDistanceKm);
  assert.ok(Number.isFinite(result.averagePathLossDb));
  assert.ok(result.averageFresnelRadiusMeters > result.usableFresnelRadiusMeters);
  assert.equal(result.usableFresnelRadiusMeters, result.averageFresnelRadiusMeters * 0.6);
});

test("provides estimated air-to-air defaults at 10,000 feet", () => {
  const nodes = applyEnvironmentDefaultsToNodes(createNetworkNodes(8), "Air to Air");
  const single = calculateMimo({
    ...DEFAULT_MIMO_INPUTS,
    environment: "Air to Air",
    txHeightFeet: AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
    rxHeightFeet: AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
  });
  const network = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, environment: "Air to Air" });
  assert.equal(AIR_TO_AIR_DEFAULT_ALTITUDE_FEET, 10_000);
  assert.equal(nodes.every((node) => node.altitudeFeet === 10_000 && node.heightGroup === "Airborne"), true);
  assert.equal(single.environment.exponent, 2);
  assert.equal(network.environment.exponent, 2);
  assert.ok(single.horizonKm > 390);
  assert.equal(network.links.every((link) => link.horizonClear), true);
});

test("assigns exact radio percentages in grouped or distributed layouts", () => {
  const nodes = createNetworkNodes(100);
  const grouped = assignRadioMix(nodes, 35, "Grouped blocks");
  const distributed = assignRadioMix(nodes, 35, "Evenly distributed");
  assert.equal(grouped.filter((node) => node.radioProfile === "sl5200").length, 35);
  assert.equal(distributed.filter((node) => node.radioProfile === "sl5200").length, 35);
  assert.equal(grouped.slice(0, 65).every((node) => node.radioProfile === "series4000"), true);
  assert.equal(grouped.slice(65).every((node) => node.radioProfile === "sl5200"), true);
  assert.equal(distributed.slice(0, 10).some((node) => node.radioProfile === "sl5200"), true);
});

test("builds repeatable uneven relay branches and calculates their maximum viable depth", () => {
  const nodes = createNetworkNodes(18);
  const first = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, topology: "Random relay chain", randomSeed: 41, trafficPerNodeMbps: 0.1 });
  const repeated = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, topology: "Random relay chain", randomSeed: 41, trafficPerNodeMbps: 0.1 });
  const shuffled = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, topology: "Random relay chain", randomSeed: 42, trafficPerNodeMbps: 0.1 });
  const loaded = calculateNetwork(nodes, { ...DEFAULT_NETWORK_INPUTS, topology: "Random relay chain", randomSeed: 41, trafficPerNodeMbps: 100 });
  assert.deepEqual(first.chainOrder, repeated.chainOrder);
  assert.deepEqual(first.chainPaths, repeated.chainPaths);
  assert.notDeepEqual(first.chainOrder, shuffled.chainOrder);
  assert.equal(first.chainOrder[0], 1);
  assert.ok(first.branchCount > 1);
  assert.equal(first.chainPaths.every((path) => path[0] === 1), true);
  assert.equal(new Set(first.chainPaths.flatMap((path) => path.slice(1))).size, 17);
  assert.equal(first.links.filter((link) => link.from === 1).length, first.branchCount);
  const branchLengths = first.chainPaths.map((path) => path.length - 1);
  assert.ok(Math.max(...branchLengths) > Math.min(...branchLengths));
  assert.equal(first.requestedChainLinks, Math.max(...branchLengths));
  for (const path of first.chainPaths) {
    const firstHop = first.links.find((link) => link.from === path[0] && link.to === path[1]);
    assert.equal(firstHop.requiredMbps, (path.length - 1) * 0.1);
  }
  assert.ok(first.maxPossibleChainLinks >= loaded.maxPossibleChainLinks);
  assert.ok(first.maxPossibleChainLinks <= first.requestedChainLinks);
});
