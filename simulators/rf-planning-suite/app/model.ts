export type EnvironmentKey =
  | "Free Space"
  | "Air to Air"
  | "Air to Ground"
  | "Maritime"
  | "Rural"
  | "Urban - Raised Antennas"
  | "Urban - Low Antennas"
  | "Ground Robotics";

export type CrossPolarization = "No" | "One Side" | "Both Sides";
export type PowerControl = "Adaptive" | "Fixed";
export type RadioProfileKey = "series4000" | "sl5200";

export const RADIO_PROFILES = {
  series4000: {
    label: "4000 Series",
    shortLabel: "4000",
    maxTxPowerW: 20,
    receivePaths: 4 as const,
    maxSpatialStreams: 2 as const,
    receiverSensitivityOffsetDb: 0,
    estimated: false,
    note: "Workbook-backed 20 W / four-path planning profile.",
    supportedRangesMHz: [[300, 6000]] as const,
    bands: [
      { label: "UHF", frequency: 435, display: "435 MHz", range: "Generic preset" },
      { label: "L-Band", frequency: 1800, display: "1.8 GHz", range: "Generic preset" },
      { label: "S-Band", frequency: 2200, display: "2.2 GHz", range: "Generic preset" },
      { label: "C-Band", frequency: 5800, display: "5.8 GHz", range: "Generic preset" },
    ],
  },
  sl5200: {
    label: "SL5200 (estimated)",
    shortLabel: "SL5200",
    maxTxPowerW: 2,
    receivePaths: 2 as const,
    maxSpatialStreams: 2 as const,
    // Calibrates the workbook curve to the published -101 dBm @ 5 MHz and
    // -107 dBm @ 1.25 MHz 1SS sensitivity anchors after the two-path offset.
    receiverSensitivityOffsetDb: -2,
    estimated: true,
    note: "Public 2 W, band, bandwidth, and sensitivity anchors with an estimated MCS curve; validate before deployment.",
    supportedRangesMHz: [[1350, 1440], [2200, 2500], [4400, 5000]] as const,
    bands: [
      { label: "L-Band", frequency: 1395, display: "1.395 GHz", range: "1350-1440 MHz" },
      { label: "S-Band", frequency: 2350, display: "2.35 GHz", range: "2200-2500 MHz" },
      { label: "C-Band", frequency: 4700, display: "4.7 GHz", range: "4400-5000 MHz" },
    ],
  },
} as const;

export const MAX_TX_HEIGHT_FEET = 30_000;
export const AIR_TO_AIR_DEFAULT_ALTITUDE_FEET = 10_000;

export type MimoInputs = {
  radioProfile: RadioProfileKey;
  environment: EnvironmentKey;
  frequencyMHz: number;
  txPowerW: number;
  bdaUsed: boolean;
  powerControl: PowerControl;
  txCableLoss: number;
  paGain: number;
  txAntennaGain: number;
  rxAntennas: 2 | 4;
  rxCableLoss: number;
  rxAntennaGain: number;
  noiseFigure: number;
  txHeightFeet: number;
  rxHeightFeet: number;
  fresnelBlocked: boolean;
  crossPolarization: CrossPolarization;
  targetDistanceKm: number;
  safetyMargin: number;
  userTrafficMbps: number;
};

type McsRow = {
  bandwidth: number;
  mcs: number;
  nss: 1 | 2;
  modulation: string;
  sensitivity: number;
  capacity: number;
};

export type ModeResult = {
  bandwidth: number;
  nss: 1 | 2;
  enabled: boolean;
  mcs: number | null;
  modulation: string;
  capacity: number | null;
  dutyCycle: number | null;
  rssi: number;
  snr: number;
  maxDistanceKm: number | null;
  viable: boolean;
  reliable: boolean;
};

export const DEFAULT_MIMO_INPUTS: MimoInputs = {
  radioProfile: "series4000",
  environment: "Rural",
  frequencyMHz: 2350,
  txPowerW: 20,
  bdaUsed: false,
  powerControl: "Adaptive",
  txCableLoss: 1,
  paGain: 0,
  txAntennaGain: 5,
  rxAntennas: 4,
  rxCableLoss: 1,
  rxAntennaGain: 5,
  noiseFigure: 5,
  txHeightFeet: 1500,
  rxHeightFeet: 6,
  fresnelBlocked: false,
  crossPolarization: "Both Sides",
  targetDistanceKm: 6,
  safetyMargin: 5,
  userTrafficMbps: 2,
};

export const ENVIRONMENTS: Record<
  EnvironmentKey,
  { exponent: number; note: string }
> = {
  "Free Space": { exponent: 2, note: "Ideal unobstructed propagation." },
  "Air to Air": { exponent: 2, note: "Estimated clear aircraft line-of-sight path; excludes maneuver, body-masking, Doppler, and atmospheric-anomaly losses." },
  "Air to Ground": { exponent: 2.1, note: "Low-loss elevated line-of-sight path." },
  Maritime: { exponent: 2.2, note: "Open-water path with sea-state sensitivity." },
  Rural: { exponent: 2.3, note: "Open terrain with moderate foliage loss." },
  "Urban - Raised Antennas": { exponent: 2.4, note: "Urban path with antennas above nearby clutter." },
  "Urban - Low Antennas": { exponent: 3.2, note: "High-loss street-level urban propagation." },
  "Ground Robotics": { exponent: 2.8, note: "Near-ground path with frequent local obstruction." },
};

const BANDWIDTHS = [20, 10, 5, 2.5, 1.25] as const;
const NOISE_FLOORS: Record<number, number> = {
  20: -96,
  10: -99,
  5: -102,
  2.5: -105,
  1.25: -108,
};

const TABLE: Record<number, { sensitivity1: number[]; capacity1: number[]; sensitivity2: number[]; capacity2: number[] }> = {
  1.25: {
    sensitivity1: [-108, -106, -103, -101, -98, -93, -91, -86],
    capacity1: [0.27, 0.55, 0.82, 1.1, 1.65, 2.2, 2.47, 2.75],
    sensitivity2: [-106, -103, -100, -97, -94, -90, -88, -83],
    capacity2: [0.55, 1.1, 1.65, 2.2, 3.3, 4.35, 4.75, 5.1],
  },
  2.5: {
    sensitivity1: [-105, -103, -100, -98, -95, -90, -88, -83],
    capacity1: [0.55, 1.1, 1.65, 2.2, 3.3, 4.4, 4.95, 5.5],
    sensitivity2: [-103, -100, -97, -94, -91, -87, -85, -80],
    capacity2: [1.1, 2.2, 3.3, 4.4, 6.6, 8.7, 9.5, 10.2],
  },
  5: {
    sensitivity1: [-102, -100, -97, -95, -92, -87, -85, -80],
    capacity1: [1.03, 2.06, 3.09, 4.12, 6.18, 8.25, 9.28, 10.3],
    sensitivity2: [-100, -97, -94, -91, -88, -84, -82, -77],
    capacity2: [2.06, 4.12, 6.18, 8.25, 12.38, 16.21, 17.62, 18.94],
  },
  10: {
    sensitivity1: [-99, -97, -94, -92, -89, -85, -83, -77],
    capacity1: [2.48, 4.96, 7.4, 9.9, 14.8, 19.9, 22.4, 24],
    sensitivity2: [-97, -94, -91, -89, -85, -82, -80, -74],
    capacity2: [4.96, 9.9, 14.8, 19.9, 29.9, 39.7, 43.5, 48.1],
  },
  20: {
    sensitivity1: [-96, -94, -91, -89, -86, -82, -80, -78],
    capacity1: [4.92, 9.82, 14.73, 19.65, 29.47, 39.29, 44.2, 47.45],
    sensitivity2: [-94, -91, -88, -86, -82, -79, -77, -75],
    capacity2: [9.82, 19.65, 29.47, 39.29, 57.04, 75, 85, 94],
  },
};

const MODULATIONS = ["BPSK 1/2", "QPSK 1/2", "QPSK 3/4", "16-QAM 1/2", "16-QAM 3/4", "64-QAM 2/3", "64-QAM 3/4", "64-QAM 5/6"];
// Exact adaptive-power curves from the supplied 4000 Series workbook.
// An external PA/BDA needs progressively more linearity backoff at higher MCS.
const RADIO_ADAPTIVE_BACKOFF = [0, 0, -1, -3, -4, -6, -6, -6];
const PA_ADAPTIVE_BACKOFF = [0, -1, -3, -4, -5, -6, -8, -10];

const MCS_ROWS: McsRow[] = BANDWIDTHS.flatMap((bandwidth) => {
  const band = TABLE[bandwidth];
  return [
    ...band.sensitivity1.map((sensitivity, index) => ({
      bandwidth,
      mcs: index,
      nss: 1 as const,
      modulation: MODULATIONS[index],
      sensitivity,
      capacity: band.capacity1[index],
    })),
    ...band.sensitivity2.map((sensitivity, index) => ({
      bandwidth,
      mcs: index + 8,
      nss: 2 as const,
      modulation: MODULATIONS[index],
      sensitivity,
      capacity: band.capacity2[index],
    })),
  ];
});

function backoffForMcs(mcs: number | null, powerControl: PowerControl, bdaUsed = false) {
  if (powerControl === "Fixed" || mcs === null) return 0;
  return (bdaUsed ? PA_ADAPTIVE_BACKOFF : RADIO_ADAPTIVE_BACKOFF)[mcs % 8];
}

function receiverPathAdjustmentDb(rxAntennas: MimoInputs["rxAntennas"]) {
  return rxAntennas === 4 ? 0 : 3;
}

function maxRangeMeters(inputs: MimoInputs, row: McsRow) {
  const exponent = ENVIRONMENTS[inputs.environment].exponent;
  const txPowerDbm = 10 * Math.log10(Math.min(inputs.txPowerW, RADIO_PROFILES[inputs.radioProfile].maxTxPowerW) * 1000);
  const receiverAdjustment = receiverPathAdjustmentDb(inputs.rxAntennas) +
    RADIO_PROFILES[inputs.radioProfile].receiverSensitivityOffsetDb;
  const sensitivity = row.sensitivity + receiverAdjustment;
  const adaptiveBackoff = backoffForMcs(row.mcs, inputs.powerControl, inputs.bdaUsed);
  const bdaGain = inputs.bdaUsed ? inputs.paGain : 0;
  const fresnelPenalty = inputs.fresnelBlocked ? 6 : 0;
  const crossPolPenalty = inputs.crossPolarization === "No" ? 0 : 3;
  const budget =
    txPowerDbm -
    inputs.txCableLoss +
    bdaGain +
    adaptiveBackoff +
    inputs.txAntennaGain -
    fresnelPenalty +
    inputs.rxAntennaGain -
    inputs.rxCableLoss -
    (inputs.noiseFigure - 5) -
    inputs.safetyMargin -
    crossPolPenalty;
  const frequencyConstant = 20 * Math.log10((4 * Math.PI * inputs.frequencyMHz) / 300);
  return Math.exp(((budget - sensitivity - frequencyConstant) * 0.2303) / exponent);
}

export function calculateMimo(inputs: MimoInputs) {
  const targetMeters = inputs.targetDistanceKm * 1000;
  const profile = RADIO_PROFILES[inputs.radioProfile];
  const frequencySupported = profile.supportedRangesMHz.some(([minimum, maximum]) => inputs.frequencyMHz >= minimum && inputs.frequencyMHz <= maximum);
  const txPowerDbm = 10 * Math.log10(Math.min(inputs.txPowerW, profile.maxTxPowerW) * 1000);
  const receiverAdjustment = receiverPathAdjustmentDb(inputs.rxAntennas) + profile.receiverSensitivityOffsetDb;
  const bdaGain = inputs.bdaUsed ? inputs.paGain : 0;
  const fresnelPenalty = inputs.fresnelBlocked ? 6 : 0;
  const crossPolPenalty = inputs.crossPolarization === "No" ? 0 : 3;
  const exponent = ENVIRONMENTS[inputs.environment].exponent;
  const frequencyConstant = 20 * Math.log10((4 * Math.PI * inputs.frequencyMHz) / 300);
  const pathLoss = frequencyConstant + exponent * 10 * Math.log10(targetMeters);

  const selected = new Map<string, { row: McsRow; maxRangeMeters: number } | null>();
  for (const bandwidth of BANDWIDTHS) {
    for (const nss of [1, 2] as const) {
      const candidates = (frequencySupported ? MCS_ROWS : [])
        .filter((row) => row.bandwidth === bandwidth && row.nss === nss)
        .map((row) => ({ row, maxRangeMeters: maxRangeMeters(inputs, row) }))
        .filter((candidate) => candidate.maxRangeMeters >= targetMeters)
        .sort((a, b) => b.row.mcs - a.row.mcs);
      selected.set(`${bandwidth}-${nss}`, candidates[0] ?? null);
    }
  }

  const modes: ModeResult[] = [];
  for (const bandwidth of BANDWIDTHS) {
    const selected1 = selected.get(`${bandwidth}-1`) ?? null;
    const selected2 = selected.get(`${bandwidth}-2`) ?? null;
    const groupBackoff =
      selected1 && selected2
        ? Math.min(
            backoffForMcs(selected1.row.mcs, inputs.powerControl, inputs.bdaUsed),
            backoffForMcs(selected2.row.mcs, inputs.powerControl, inputs.bdaUsed),
          )
        : 0;
    const rssi =
      txPowerDbm -
      inputs.txCableLoss +
      bdaGain +
      groupBackoff +
      inputs.txAntennaGain -
      fresnelPenalty +
      inputs.rxAntennaGain -
      inputs.rxCableLoss -
      (inputs.noiseFigure - 5) -
      inputs.safetyMargin -
      crossPolPenalty -
      pathLoss;
    const snr = rssi - (NOISE_FLOORS[bandwidth] + receiverAdjustment);

    for (const nss of [2, 1] as const) {
      const match = nss === 1 ? selected1 : selected2;
      const enabled = nss === 1 || inputs.crossPolarization === "Both Sides";
      const capacity = match?.row.capacity ?? null;
      const dutyCycle = capacity ? (inputs.userTrafficMbps / capacity) * 100 : null;
      const viable = Boolean(enabled && match && snr >= 0 && dutyCycle !== null && dutyCycle <= 100);
      const reliable = Boolean(viable && snr >= 5 && dutyCycle !== null && dutyCycle <= 80);
      modes.push({
        bandwidth,
        nss,
        enabled,
        mcs: match?.row.mcs ?? null,
        modulation: match?.row.modulation ?? "No link",
        capacity,
        dutyCycle,
        rssi,
        snr,
        maxDistanceKm: match ? match.maxRangeMeters / 1000 : null,
        viable,
        reliable,
      });
    }
  }

  const txHeightMeters = inputs.txHeightFeet * 0.3048;
  const rxHeightMeters = inputs.rxHeightFeet * 0.3048;
  const horizonKm = 3.57 * Math.sqrt(txHeightMeters) + 3.57 * Math.sqrt(rxHeightMeters);
  const fresnelRadiusMeters = 8.657 * Math.sqrt(inputs.targetDistanceKm / (inputs.frequencyMHz / 1000));
  const usableFresnelRadiusMeters = fresnelRadiusMeters * 0.6;
  const ranked = [...modes]
    .filter((mode) => mode.enabled && mode.capacity !== null)
    .sort((a, b) => {
      const aRank = a.reliable ? 2 : a.viable ? 1 : 0;
      const bRank = b.reliable ? 2 : b.viable ? 1 : 0;
      return bRank - aRank || (b.capacity ?? 0) - (a.capacity ?? 0);
    });
  const bestMode = ranked[0] ?? null;
  const overallStatus: "Reliable" | "Possible" | "No viable mode" = modes.some((mode) => mode.reliable)
    ? "Reliable"
    : modes.some((mode) => mode.viable)
      ? "Possible"
      : "No viable mode";

  return {
    modes,
    bestMode,
    overallStatus,
    pathLoss,
    horizonKm,
    fresnelRadiusMeters,
    usableFresnelRadiusMeters,
    horizonClear: horizonKm >= inputs.targetDistanceKm,
    environment: ENVIRONMENTS[inputs.environment],
    frequencySupported,
  };
}

export type NetworkTopology = "Direct to hub" | "Relay chain" | "Random relay chain";
export type HeightGroup = "Ground" | "Low" | "Elevated" | "Airborne";
export type NetworkLinkStatus = "Reliable" | "Possible" | "Invalid";
export type RadioMixPlacement = "Grouped blocks" | "Evenly distributed";

export type NetworkNode = {
  id: number;
  name: string;
  radioProfile: RadioProfileKey;
  altitudeFeet: number;
  heightGroup: HeightGroup;
};

export type NetworkInputs = {
  topology: NetworkTopology;
  environment: EnvironmentKey;
  frequencyMHz: number;
  bandwidthMHz: (typeof BANDWIDTHS)[number];
  averageDistanceKm: number;
  trafficPerNodeMbps: number;
  antennaGainDbi: number;
  cableLossDb: number;
  safetyMarginDb: number;
  fresnelBlocked: boolean;
  crossPolarization: CrossPolarization;
  randomSeed: number;
};

export type NetworkLinkResult = {
  id: string;
  from: number;
  to: number;
  distanceKm: number;
  requiredMbps: number;
  capacityMbps: number | null;
  snrDb: number;
  mcs: number | null;
  spatialStreams: 1 | 2 | null;
  horizonClear: boolean;
  status: NetworkLinkStatus;
};

export const DEFAULT_NETWORK_INPUTS: NetworkInputs = {
  topology: "Direct to hub",
  environment: "Rural",
  frequencyMHz: 2350,
  bandwidthMHz: 10,
  averageDistanceKm: 5,
  trafficPerNodeMbps: 0.0288,
  antennaGainDbi: 5,
  cableLossDb: 1,
  safetyMarginDb: 5,
  fresnelBlocked: false,
  crossPolarization: "Both Sides",
  randomSeed: 2026,
};

export function calculatePacketTrafficMbps(messageSizeKb: number, messagesPerSecond: number, overheadPercent: number) {
  const payloadMbps = Math.max(0, messageSizeKb) * 8 * Math.max(0, messagesPerSecond) / 1000;
  return payloadMbps * (1 + Math.max(0, overheadPercent) / 100);
}

export const HEIGHT_GROUP_DEFAULTS: Record<HeightGroup, number> = {
  Ground: 6,
  Low: 50,
  Elevated: 500,
  Airborne: AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
};

export function createNetworkNodes(count: number, radioProfile: RadioProfileKey = "series4000") {
  const safeCount = Math.min(150, Math.max(2, Math.round(count)));
  return Array.from({ length: safeCount }, (_, index): NetworkNode => ({
    id: index + 1,
    name: index === 0 ? "Gateway" : `Node ${String(index + 1).padStart(3, "0")}`,
    radioProfile,
    altitudeFeet: index === 0 ? HEIGHT_GROUP_DEFAULTS.Elevated : HEIGHT_GROUP_DEFAULTS.Ground,
    heightGroup: index === 0 ? "Elevated" : "Ground",
  }));
}

export function applyEnvironmentDefaultsToNodes(nodes: NetworkNode[], environment: EnvironmentKey) {
  if (environment !== "Air to Air") return nodes;
  return nodes.map((node) => ({
    ...node,
    altitudeFeet: AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
    heightGroup: "Airborne" as const,
  }));
}

export function assignRadioMix(nodes: NetworkNode[], sl5200Percent: number, placement: RadioMixPlacement) {
  const boundedPercent = Math.min(100, Math.max(0, sl5200Percent));
  const sl5200Count = Math.round(nodes.length * boundedPercent / 100);
  const series4000Count = nodes.length - sl5200Count;
  return nodes.map((node, index) => {
    const isSl5200 = placement === "Grouped blocks"
      ? index >= series4000Count
      : Math.floor(((index + 1) * sl5200Count) / nodes.length) > Math.floor((index * sl5200Count) / nodes.length);
    return { ...node, radioProfile: isSl5200 ? "sl5200" : "series4000" };
  });
}

function seededRandom(seed: number) {
  let state = Math.max(1, Math.round(seed)) >>> 0;
  return () => {
    state += 0x6d2b79f5;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

function seededShuffle(nodes: NetworkNode[], seed: number) {
  const shuffled = [...nodes];
  const random = seededRandom(seed);
  for (let index = shuffled.length - 1; index > 0; index -= 1) {
    const target = Math.floor(random() * (index + 1));
    [shuffled[index], shuffled[target]] = [shuffled[target], shuffled[index]];
  }
  return shuffled;
}

function buildRandomBranches(nodes: NetworkNode[], randomSeed: number) {
  if (nodes.length < 2) return [] as NetworkNode[][];
  const shuffled = seededShuffle(nodes.slice(1), randomSeed);
  const branchCount = Math.min(shuffled.length, Math.max(2, Math.min(8, Math.round(Math.sqrt(shuffled.length)))));
  const branches = Array.from({ length: branchCount }, (_, index) => [shuffled[index]]);
  const random = seededRandom(randomSeed ^ 0x9e3779b9);

  for (const node of shuffled.slice(branchCount)) {
    const weights = branches.map((branch) => (branch.length + 0.75) ** 1.35);
    const totalWeight = weights.reduce((total, weight) => total + weight, 0);
    let selection = random() * totalWeight;
    let branchIndex = 0;
    for (; branchIndex < weights.length - 1; branchIndex += 1) {
      selection -= weights[branchIndex];
      if (selection <= 0) break;
    }
    branches[branchIndex].push(node);
  }

  const lengths = branches.map((branch) => branch.length);
  if (branches.length > 1 && Math.max(...lengths) === Math.min(...lengths) && lengths[0] > 1) {
    branches[0].push(branches.at(-1)!.pop()!);
  }
  return branches;
}

function buildNetworkPlan(nodes: NetworkNode[], topology: NetworkTopology, randomSeed: number) {
  const gateway = nodes[0];
  if (!gateway) return { edges: [], paths: [] as NetworkNode[][], order: [] as NetworkNode[] };

  if (topology === "Direct to hub") {
    const paths = nodes.slice(1).map((node) => [gateway, node]);
    return {
      paths,
      order: nodes,
      edges: paths.map((path) => ({ from: path[0], to: path[1], requiredNodes: 1 })),
    };
  }

  const branches = topology === "Random relay chain"
    ? buildRandomBranches(nodes, randomSeed)
    : [nodes.slice(1)];
  const paths = branches.map((branch) => [gateway, ...branch]);
  return {
    paths,
    order: [gateway, ...branches.flat()],
    edges: paths.flatMap((path) => path.slice(1).map((node, index) => ({
      from: path[index],
      to: node,
      requiredNodes: path.length - index - 1,
    }))),
  };
}

function directionalMode(
  transmitter: NetworkNode,
  receiver: NetworkNode,
  inputs: NetworkInputs,
  distanceMeters: number,
  nss: 1 | 2,
) {
  const txProfile = RADIO_PROFILES[transmitter.radioProfile];
  const rxProfile = RADIO_PROFILES[receiver.radioProfile];
  const frequencySupported = (profile: (typeof RADIO_PROFILES)[RadioProfileKey]) =>
    profile.supportedRangesMHz.some(([minimum, maximum]) => inputs.frequencyMHz >= minimum && inputs.frequencyMHz <= maximum);
  if (!frequencySupported(txProfile) || !frequencySupported(rxProfile)) return null;
  const receiverAdjustment = (rxProfile.receivePaths === 4 ? 0 : 3) + rxProfile.receiverSensitivityOffsetDb;
  const exponent = ENVIRONMENTS[inputs.environment].exponent;
  const frequencyConstant = 20 * Math.log10((4 * Math.PI * inputs.frequencyMHz) / 300);
  const pathLoss = frequencyConstant + exponent * 10 * Math.log10(distanceMeters);
  const txPowerDbm = 10 * Math.log10(txProfile.maxTxPowerW * 1000);
  const fresnelPenalty = inputs.fresnelBlocked ? 6 : 0;
  const crossPolPenalty = inputs.crossPolarization === "No" ? 0 : 3;
  const selected = new Map<1 | 2, McsRow | null>();
  for (const candidateNss of [1, 2] as const) {
    const candidate = MCS_ROWS
      .filter((row) => row.bandwidth === inputs.bandwidthMHz && row.nss === candidateNss)
      .map((row) => {
        const rssi =
          txPowerDbm +
          backoffForMcs(row.mcs, "Adaptive") +
          inputs.antennaGainDbi * 2 -
          inputs.cableLossDb * 2 -
          inputs.safetyMarginDb -
          fresnelPenalty -
          crossPolPenalty -
          pathLoss;
        return { row, closes: rssi > row.sensitivity + receiverAdjustment };
      })
      .filter((entry) => entry.closes)
      .sort((a, b) => b.row.mcs - a.row.mcs)[0];
    selected.set(candidateNss, candidate?.row ?? null);
  }

  const selected1 = selected.get(1) ?? null;
  const selected2 = selected.get(2) ?? null;
  const groupBackoff = selected1 && selected2
    ? Math.min(backoffForMcs(selected1.mcs, "Adaptive"), backoffForMcs(selected2.mcs, "Adaptive"))
    : 0;
  const row = selected.get(nss) ?? null;
  if (!row || (nss === 2 && inputs.crossPolarization !== "Both Sides")) return null;
  const rssi =
    txPowerDbm +
    groupBackoff +
    inputs.antennaGainDbi * 2 -
    inputs.cableLossDb * 2 -
    inputs.safetyMarginDb -
    fresnelPenalty -
    crossPolPenalty -
    pathLoss;
  return {
    row,
    rssi,
    snr: rssi - (NOISE_FLOORS[inputs.bandwidthMHz] + receiverAdjustment),
  };
}

function evaluateNetworkLink(from: NetworkNode, to: NetworkNode, inputs: NetworkInputs, requiredMbps: number): NetworkLinkResult {
  const verticalKm = Math.abs(from.altitudeFeet - to.altitudeFeet) * 0.0003048;
  const distanceKm = Math.sqrt(inputs.averageDistanceKm ** 2 + verticalKm ** 2);
  const distanceMeters = distanceKm * 1000;
  const fromHeightMeters = Math.max(1, from.altitudeFeet) * 0.3048;
  const toHeightMeters = Math.max(1, to.altitudeFeet) * 0.3048;
  const horizonKm = 3.57 * Math.sqrt(fromHeightMeters) + 3.57 * Math.sqrt(toHeightMeters);
  const horizonClear = horizonKm >= distanceKm;
  const maxStreams = Math.min(
    RADIO_PROFILES[from.radioProfile].maxSpatialStreams,
    RADIO_PROFILES[to.radioProfile].maxSpatialStreams,
  );

  const bidirectional = ([2, 1] as const)
    .filter((nss) => nss <= maxStreams)
    .map((nss) => {
      const forward = directionalMode(from, to, inputs, distanceMeters, nss);
      const reverse = directionalMode(to, from, inputs, distanceMeters, nss);
      if (!forward || !reverse) return null;
      return {
        nss,
        capacity: Math.min(forward.row.capacity, reverse.row.capacity),
        snr: Math.min(forward.snr, reverse.snr),
        mcs: Math.min(forward.row.mcs, reverse.row.mcs),
      };
    })
    .filter((mode): mode is NonNullable<typeof mode> => mode !== null)
    .sort((a, b) => b.capacity - a.capacity);

  const best = bidirectional[0] ?? null;
  const capacityMbps = best?.capacity ?? null;
  const viable = Boolean(horizonClear && capacityMbps !== null && capacityMbps >= requiredMbps);
  const reliable = Boolean(viable && best && best.snr >= 5 && capacityMbps! >= requiredMbps * 1.25);

  return {
    id: `${from.id}-${to.id}`,
    from: from.id,
    to: to.id,
    distanceKm,
    requiredMbps,
    capacityMbps,
    snrDb: best?.snr ?? -99,
    mcs: best?.mcs ?? null,
    spatialStreams: best?.nss ?? null,
    horizonClear,
    status: reliable ? "Reliable" : viable ? "Possible" : "Invalid",
  };
}

export function calculateNetwork(nodes: NetworkNode[], inputs: NetworkInputs) {
  const safeNodes = nodes.slice(0, 150);
  const plan = buildNetworkPlan(safeNodes, inputs.topology, inputs.randomSeed);
  const links = plan.edges.map((edge) =>
    evaluateNetworkLink(
      edge.from,
      edge.to,
      inputs,
      edge.requiredNodes * inputs.trafficPerNodeMbps,
    ),
  );

  const adjacency = new Map<number, number[]>();
  for (const node of safeNodes) adjacency.set(node.id, []);
  for (const link of links.filter((candidate) => candidate.status !== "Invalid")) {
    adjacency.get(link.from)?.push(link.to);
    adjacency.get(link.to)?.push(link.from);
  }
  const connected = new Set<number>();
  const queue = safeNodes[0] ? [safeNodes[0].id] : [];
  while (queue.length) {
    const current = queue.shift()!;
    if (connected.has(current)) continue;
    connected.add(current);
    for (const neighbor of adjacency.get(current) ?? []) {
      if (!connected.has(neighbor)) queue.push(neighbor);
    }
  }

  const invalidLinks = links.filter((link) => link.status === "Invalid").length;
  const possibleLinks = links.filter((link) => link.status === "Possible").length;
  const reliableLinks = links.filter((link) => link.status === "Reliable").length;
  const capacities = links.map((link) => link.capacityMbps).filter((capacity): capacity is number => capacity !== null);
  const bottleneckCapacityMbps = capacities.length ? Math.min(...capacities) : null;
  const weakestSnrDb = links.length ? Math.min(...links.map((link) => link.snrDb)) : null;
  const totalTrafficMbps = Math.max(0, safeNodes.length - 1) * inputs.trafficPerNodeMbps;
  const nodeDutyCycles = new Map(safeNodes.map((node) => [node.id, 0]));
  for (const link of links) {
    const linkDutyCycle = link.capacityMbps && link.capacityMbps > 0
      ? (link.requiredMbps / link.capacityMbps) * 100
      : Number.POSITIVE_INFINITY;
    nodeDutyCycles.set(link.from, (nodeDutyCycles.get(link.from) ?? 0) + linkDutyCycle);
    nodeDutyCycles.set(link.to, (nodeDutyCycles.get(link.to) ?? 0) + linkDutyCycle);
  }
  const gatewayDutyCyclePercent = safeNodes[0]
    ? nodeDutyCycles.get(safeNodes[0].id) ?? 0
    : 0;
  const mostLoadedNode = [...nodeDutyCycles.entries()].sort((a, b) => b[1] - a[1])[0] ?? null;
  const maxNodeDutyCyclePercent = mostLoadedNode?.[1] ?? 0;
  const airtimeOverloaded = maxNodeDutyCyclePercent > 100;
  const airtimeMarginal = maxNodeDutyCyclePercent > 80;
  const linksById = new Map(links.map((link) => [link.id, link]));
  const requestedChainLinks = inputs.topology === "Direct to hub"
    ? null
    : Math.max(0, ...plan.paths.map((path) => path.length - 1));
  const maxPossibleChainLinks = inputs.topology === "Direct to hub"
    ? null
    : Math.max(0, ...plan.paths.map((path) => {
        let viableHops = 0;
        for (let index = 1; index < path.length; index += 1) {
          const link = linksById.get(`${path[index - 1].id}-${path[index].id}`);
          const upstreamDutyCycle = nodeDutyCycles.get(path[index - 1].id) ?? Number.POSITIVE_INFINITY;
          const downstreamDutyCycle = nodeDutyCycles.get(path[index].id) ?? Number.POSITIVE_INFINITY;
          if (!link || link.status === "Invalid" || upstreamDutyCycle > 100 || downstreamDutyCycle > 100) break;
          viableHops += 1;
        }
        return viableHops;
      }));
  const averageLinkDistanceKm = links.length
    ? links.reduce((total, link) => total + link.distanceKm, 0) / links.length
    : inputs.averageDistanceKm;
  const environment = ENVIRONMENTS[inputs.environment];
  const frequencyConstant = 20 * Math.log10((4 * Math.PI * inputs.frequencyMHz) / 300);
  const averagePathLossDb = frequencyConstant + environment.exponent * 10 * Math.log10(averageLinkDistanceKm * 1000);
  const averageFresnelRadiusMeters = 8.657 * Math.sqrt(averageLinkDistanceKm / (inputs.frequencyMHz / 1000));
  const unsupportedFrequencyNodes = safeNodes.filter((node) =>
    !RADIO_PROFILES[node.radioProfile].supportedRangesMHz.some(([minimum, maximum]) => inputs.frequencyMHz >= minimum && inputs.frequencyMHz <= maximum),
  ).length;
  const status: NetworkLinkStatus =
    invalidLinks > 0 || connected.size < safeNodes.length || airtimeOverloaded
      ? "Invalid"
      : possibleLinks > 0 || airtimeMarginal
        ? "Possible"
        : "Reliable";

  return {
    status,
    links,
    connectedNodes: connected.size,
    connectedNodeIds: [...connected],
    reliableLinks,
    possibleLinks,
    invalidLinks,
    bottleneckCapacityMbps,
    weakestSnrDb,
    totalTrafficMbps,
    gatewayDutyCyclePercent,
    maxNodeDutyCyclePercent,
    mostLoadedNodeId: mostLoadedNode?.[0] ?? null,
    maxPossibleChainLinks,
    requestedChainLinks,
    branchCount: inputs.topology === "Direct to hub" ? safeNodes.length - 1 : plan.paths.length,
    chainOrder: inputs.topology === "Direct to hub" ? [] : plan.order.map((node) => node.id),
    chainPaths: inputs.topology === "Direct to hub" ? [] : plan.paths.map((path) => path.map((node) => node.id)),
    averageLinkDistanceKm,
    averagePathLossDb,
    averageFresnelRadiusMeters,
    usableFresnelRadiusMeters: averageFresnelRadiusMeters * 0.6,
    environment,
    unsupportedFrequencyNodes,
    nodeCount: safeNodes.length,
    recommendation:
      unsupportedFrequencyNodes > 0
        ? `${unsupportedFrequencyNodes} node${unsupportedFrequencyNodes === 1 ? "" : "s"} use a radio profile that does not support ${inputs.frequencyMHz} MHz. Change frequency or radio assignment.`
        : airtimeOverloaded
          ? `Node ${mostLoadedNode?.[0] ?? 1} requires ${Number.isFinite(maxNodeDutyCyclePercent) ? maxNodeDutyCyclePercent.toFixed(0) : "more than 100"}% modeled half-duplex airtime. Reduce traffic, add branches or gateways, or increase link capacity.`
        : airtimeMarginal
          ? `Node ${mostLoadedNode?.[0] ?? 1} requires ${maxNodeDutyCyclePercent.toFixed(0)}% modeled half-duplex airtime, above the workbook's 80% reliable-planning limit.`
        : status === "Reliable"
        ? "All planned paths close with reserve margin. Confirm terrain and antenna placement before deployment."
        : status === "Possible"
          ? "Every node connects, but at least one path has limited SNR or traffic headroom."
          : inputs.topology !== "Direct to hub"
            ? "One or more relay hops fail or overload. Shorten spacing, reduce traffic, raise nodes, or use higher-power radios near the gateway."
            : "One or more direct paths fail. Shorten hub radius, raise the affected nodes, improve antennas, or change their radio profile.",
  };
}
