"use client";

import { useMemo, useState } from "react";
import NetworkMap from "./NetworkMap";
import {
  AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
  applyEnvironmentDefaultsToNodes,
  assignRadioMix,
  calculateNetwork,
  calculatePacketTrafficMbps,
  createNetworkNodes,
  DEFAULT_NETWORK_INPUTS,
  ENVIRONMENTS,
  HEIGHT_GROUP_DEFAULTS,
  MAX_TX_HEIGHT_FEET,
  RADIO_PROFILES,
  type EnvironmentKey,
  type CrossPolarization,
  type HeightGroup,
  type NetworkInputs,
  type NetworkNode,
  type NetworkTopology,
  type RadioProfileKey,
  type RadioMixPlacement,
} from "./model";

const PAGE_SIZE = 25;
const HEIGHT_GROUPS = Object.keys(HEIGHT_GROUP_DEFAULTS) as HeightGroup[];
const RADIO_KEYS = Object.keys(RADIO_PROFILES) as RadioProfileKey[];
const EXTENSIVE_TEST_TRAFFIC_MBPS = 5.5;
const TOPOLOGY_OPTIONS: Array<{ label: string; value: NetworkTopology }> = [
  { label: "Direct links", value: "Direct to hub" },
  { label: "Chain link", value: "Relay chain" },
  { label: "Random branches", value: "Random relay chain" },
];

function topologyLabel(topology: NetworkTopology) {
  return TOPOLOGY_OPTIONS.find((option) => option.value === topology)?.label ?? topology;
}

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function NetworkNumber({
  id,
  label,
  unit,
  value,
  min,
  max,
  step,
  onChange,
}: {
  id: string;
  label: string;
  unit: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (value: number) => void;
}) {
  return (
    <label className="network-field" htmlFor={id}>
      <span>{label}</span>
      <div><input id={id} max={max} min={min} onChange={(event) => onChange(clamp(Number(event.target.value), min, max))} step={step} type="number" value={value} /><small>{unit}</small></div>
    </label>
  );
}

function NetworkSelect({
  id,
  label,
  value,
  options,
  onChange,
}: {
  id: string;
  label: string;
  value: string;
  options: Array<{ label: string; value: string }>;
  onChange: (value: string) => void;
}) {
  return (
    <label className="network-field" htmlFor={id}>
      <span>{label}</span>
      <select id={id} onChange={(event) => onChange(event.target.value)} value={value}>
        {options.map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
      </select>
    </label>
  );
}

export default function MultiNodePlanner() {
  const [inputs, setInputs] = useState<NetworkInputs>(DEFAULT_NETWORK_INPUTS);
  const [nodes, setNodes] = useState<NetworkNode[]>(() => createNetworkNodes(8));
  const [nodePage, setNodePage] = useState(0);
  const [linkPage, setLinkPage] = useState(0);
  const [bulkProfile, setBulkProfile] = useState<RadioProfileKey>("series4000");
  const [bulkGroup, setBulkGroup] = useState<HeightGroup>("Ground");
  const [bulkAltitude, setBulkAltitude] = useState(HEIGHT_GROUP_DEFAULTS.Ground);
  const [sl5200Percent, setSl5200Percent] = useState(25);
  const [mixPlacement, setMixPlacement] = useState<RadioMixPlacement>("Grouped blocks");
  const [trafficMode, setTrafficMode] = useState<"Typical" | "Extensive">("Typical");
  const [messageSizeKb, setMessageSizeKb] = useState(3);
  const [messagesPerSecond, setMessagesPerSecond] = useState(1);
  const [trafficOverheadPercent, setTrafficOverheadPercent] = useState(20);
  const typicalTrafficMbps = calculatePacketTrafficMbps(messageSizeKb, messagesPerSecond, trafficOverheadPercent);
  const modeledTrafficPerNodeMbps = typicalTrafficMbps + (trafficMode === "Extensive" ? EXTENSIVE_TEST_TRAFFIC_MBPS : 0);
  const results = useMemo(
    () => calculateNetwork(nodes, { ...inputs, trafficPerNodeMbps: modeledTrafficPerNodeMbps }),
    [inputs, modeledTrafficPerNodeMbps, nodes],
  );
  const draftMapSignature = useMemo(
    () => JSON.stringify({ inputs, modeledTrafficPerNodeMbps, nodes }),
    [inputs, modeledTrafficPerNodeMbps, nodes],
  );
  const [appliedMap, setAppliedMap] = useState(() => ({
    layoutSeed: inputs.randomSeed,
    links: results.links,
    nodes,
    status: results.status,
    topology: inputs.topology,
  }));
  const [appliedMapSignature, setAppliedMapSignature] = useState(draftMapSignature);
  const mapHasPendingChanges = draftMapSignature !== appliedMapSignature;
  const networkBands = nodes.some((node) => node.radioProfile === "sl5200") ? RADIO_PROFILES.sl5200.bands : RADIO_PROFILES.series4000.bands;
  const tone = results.status === "Reliable" ? "good" : results.status === "Possible" ? "moderate" : "weak";
  const nodePageCount = Math.max(1, Math.ceil(nodes.length / PAGE_SIZE));
  const safeNodePage = Math.min(nodePage, nodePageCount - 1);
  const connectedNodeIds = new Set(results.connectedNodeIds);
  const linksByNode = new Map<number, typeof results.links>();
  for (const node of nodes) linksByNode.set(node.id, []);
  for (const link of results.links) {
    linksByNode.get(link.from)?.push(link);
    linksByNode.get(link.to)?.push(link);
  }
  const linkRank = { Invalid: 0, Possible: 1, Reliable: 2 } as const;
  const rankedLinks = [...results.links].sort((a, b) => linkRank[a.status] - linkRank[b.status] || a.snrDb - b.snrDb);
  const linkPageCount = Math.max(1, Math.ceil(rankedLinks.length / PAGE_SIZE));
  const safeLinkPage = Math.min(linkPage, linkPageCount - 1);
  const nodesById = new Map(nodes.map((node) => [node.id, node]));
  const currentSl5200Count = nodes.filter((node) => node.radioProfile === "sl5200").length;
  const current4000Count = nodes.length - currentSl5200Count;
  const targetSl5200Count = Math.round(nodes.length * sl5200Percent / 100);
  const target4000Count = nodes.length - targetSl5200Count;

  const updateInput = <K extends keyof NetworkInputs>(field: K, value: NetworkInputs[K]) => {
    setInputs((current) => ({ ...current, [field]: value }));
  };

  const applyEnvironment = (environment: EnvironmentKey) => {
    setInputs((current) => ({
      ...current,
      environment,
      fresnelBlocked: environment === "Air to Air" ? false : current.fresnelBlocked,
    }));
    if (environment === "Air to Air") {
      setNodes((current) => applyEnvironmentDefaultsToNodes(current, environment));
      setBulkGroup("Airborne");
      setBulkAltitude(AIR_TO_AIR_DEFAULT_ALTITUDE_FEET);
    }
  };

  const resizeNodes = (requestedCount: number) => {
    const count = clamp(Math.round(requestedCount), 2, 150);
    setNodes((current) => {
      const defaults = createNetworkNodes(count, bulkProfile);
      return defaults.map((node, index) => current[index] ?? { ...node, radioProfile: bulkProfile, heightGroup: bulkGroup, altitudeFeet: bulkAltitude });
    });
    setNodePage((current) => Math.min(current, Math.ceil(count / PAGE_SIZE) - 1));
  };

  const updateNode = (id: number, update: Partial<NetworkNode>) => {
    setNodes((current) => current.map((node) => node.id === id ? { ...node, ...update } : node));
  };

  const applyMapChanges = () => {
    setAppliedMap({
      layoutSeed: inputs.randomSeed,
      links: results.links,
      nodes,
      status: results.status,
      topology: inputs.topology,
    });
    setAppliedMapSignature(draftMapSignature);
  };

  const resetNetwork = () => {
    setInputs(DEFAULT_NETWORK_INPUTS);
    setNodes(createNetworkNodes(8));
    setNodePage(0);
    setLinkPage(0);
    setBulkProfile("series4000");
    setBulkGroup("Ground");
    setBulkAltitude(HEIGHT_GROUP_DEFAULTS.Ground);
    setSl5200Percent(25);
    setMixPlacement("Grouped blocks");
    setTrafficMode("Typical");
    setMessageSizeKb(3);
    setMessagesPerSecond(1);
    setTrafficOverheadPercent(20);
  };

  return (
    <section className="network-planner" aria-label="Multi-node network planner">
      <section className={`network-assessment network-assessment--${tone}`} aria-live="polite">
        <div className="network-assessment__title"><span>Network assessment</span><h2>{results.status}</h2><p>{results.connectedNodes} of {results.nodeCount} nodes connected to the gateway.</p></div>
        <div className="network-assessment__recommendation"><span>Recommendation</span><h3>{results.status === "Reliable" ? "Network plan has reserve" : results.status === "Possible" ? "Review marginal paths" : "Revise failed paths"}</h3><p>{results.recommendation}</p></div>
        <div className="network-assessment__metrics">
          <div><span>Connected</span><strong>{results.connectedNodes}/{results.nodeCount}</strong><small>nodes</small></div>
          <div><span>Gateway airtime</span><strong>{Number.isFinite(results.gatewayDutyCyclePercent) ? results.gatewayDutyCyclePercent.toFixed(0) : ">100"}</strong><small>% modeled</small></div>
          <div><span>Invalid links</span><strong>{results.invalidLinks}</strong><small>paths</small></div>
        </div>
      </section>

      <section className="network-geometry-bar" aria-label="Network topology and average distance controls">
        <div className="network-geometry-heading">
          <span className="section-kicker">Network geometry</span>
          <h2>Topology &amp; spacing</h2>
          <p>Set how nodes connect and the average ground distance modeled between linked nodes.</p>
        </div>
        <fieldset className="network-topology-picker">
          <legend>Chain-link mode</legend>
          <div>
            {TOPOLOGY_OPTIONS.map((option) => (
              <button aria-pressed={inputs.topology === option.value} key={option.value} onClick={() => updateInput("topology", option.value)} type="button">{option.label}</button>
            ))}
          </div>
          <small>{inputs.topology === "Direct to hub" ? "Every node links directly to the gateway." : inputs.topology === "Relay chain" ? "Nodes relay in one sequence toward the gateway." : "Multiple repeatable relay branches radiate from the gateway with uneven chain lengths."}</small>
        </fieldset>
        <div className="network-distance-control">
          <NetworkNumber id="network-distance" label="Average distance between nodes" max={100} min={0.1} onChange={(value) => updateInput("averageDistanceKm", value)} step={0.1} unit="km" value={inputs.averageDistanceKm} />
          <input aria-label="Average distance between nodes slider" className="network-range" max={100} min={0.1} onChange={(event) => updateInput("averageDistanceKm", Number(event.target.value))} step={0.1} type="range" value={inputs.averageDistanceKm} />
        </div>
      </section>

      <div className="network-workspace">
        <aside className="network-controls">
          <details className="network-control-card">
            <summary className="network-control-heading"><div className="network-control-title"><span>01</span><h2>Network setup</h2></div><p>Node count, packet load, and shared RF assumptions</p><div className="network-control-summary"><span>{nodes.length} nodes</span><span>{topologyLabel(inputs.topology)}</span><span>{trafficMode} test</span><span>{modeledTrafficPerNodeMbps.toFixed(3)} Mbps/node</span></div><i aria-hidden="true" className="network-control-toggle" /></summary>
            <div className="network-control-body">
              <NetworkNumber id="network-node-count" label="Node count" max={150} min={2} onChange={resizeNodes} step={1} unit="nodes" value={nodes.length} />
              <input aria-label="Node count slider" className="network-range" max={150} min={2} onChange={(event) => resizeNodes(Number(event.target.value))} step={1} type="range" value={nodes.length} />
              {inputs.topology === "Random relay chain" ? <div className="network-random-chain">
                <NetworkNumber id="network-random-seed" label="Random branch seed" max={999999} min={1} onChange={(value) => updateInput("randomSeed", Math.round(value))} step={1} unit="seed" value={inputs.randomSeed} />
                <button className="network-apply-button" onClick={() => updateInput("randomSeed", inputs.randomSeed >= 999999 ? 1 : inputs.randomSeed + 1)} type="button">Reshuffle branches</button>
                <div><span>Current branch paths · {results.branchCount} chains</span><p>{results.chainPaths.slice(0, 8).map((path) => path.join(" → ")).join("  |  ")}{results.chainPaths.length > 8 ? "  |  …" : ""}</p></div>
              </div> : null}
              <NetworkSelect id="network-environment" label="Environment" onChange={(value) => applyEnvironment(value as EnvironmentKey)} options={Object.keys(ENVIRONMENTS).map((value) => ({ label: value, value }))} value={inputs.environment} />
              <div className="network-environment-note"><span>n = {results.environment.exponent.toFixed(1)}</span>{results.environment.note}</div>
              <div className="network-band-picker" aria-label="Multi-node radio-band presets">
                <span>{nodes.some((node) => node.radioProfile === "sl5200") ? "SL5200-compatible bands" : "Quick radio bands"}</span>
                <div>
                  {networkBands.map((band) => (
                    <button aria-pressed={inputs.frequencyMHz === band.frequency} key={band.label} onClick={() => updateInput("frequencyMHz", band.frequency)} type="button">
                      <strong>{band.label}</strong><small>{band.display}</small><em>{band.range}</em>
                    </button>
                  ))}
                </div>
              </div>
              <NetworkSelect id="network-bandwidth" label="Channel bandwidth" onChange={(value) => updateInput("bandwidthMHz", Number(value) as NetworkInputs["bandwidthMHz"])} options={[20, 10, 5, 2.5, 1.25].map((value) => ({ label: `${value} MHz`, value: String(value) }))} value={String(inputs.bandwidthMHz)} />
              <NetworkNumber id="network-frequency" label="Center frequency" max={6000} min={300} onChange={(value) => updateInput("frequencyMHz", value)} step={5} unit="MHz" value={inputs.frequencyMHz} />
              <section className="network-traffic-model" aria-label="Per-node traffic test model">
                <div className="network-traffic-heading"><span>Traffic test profile</span><p>Traffic generated by each non-gateway node and accumulated toward the gateway.</p></div>
                <div className="network-traffic-mode" aria-label="Traffic test mode">
                  <button aria-pressed={trafficMode === "Typical"} onClick={() => setTrafficMode("Typical")} type="button">Typical packets</button>
                  <button aria-pressed={trafficMode === "Extensive"} onClick={() => setTrafficMode("Extensive")} type="button">Extensive +5.5 Mbps</button>
                </div>
                <div className="network-traffic-fields">
                  <NetworkNumber id="network-message-size" label="Typical message size" max={1024} min={0.1} onChange={setMessageSizeKb} step={0.1} unit="kB" value={messageSizeKb} />
                  <NetworkNumber id="network-message-rate" label="Messages per second" max={1000} min={0.01} onChange={setMessagesPerSecond} step={0.01} unit="msg/s" value={messagesPerSecond} />
                  <NetworkNumber id="network-traffic-overhead" label="Planning overhead" max={200} min={0} onChange={setTrafficOverheadPercent} step={1} unit="%" value={trafficOverheadPercent} />
                </div>
                <div className={`network-traffic-result${trafficMode === "Extensive" ? " network-traffic-result--extensive" : ""}`}>
                  <span>Modeled traffic per node</span>
                  <strong>{modeledTrafficPerNodeMbps.toFixed(4)} Mbps</strong>
                  <small>{typicalTrafficMbps.toFixed(4)} Mbps packet load{trafficMode === "Extensive" ? ` + ${EXTENSIVE_TEST_TRAFFIC_MBPS.toFixed(1)} Mbps extensive load` : ""}</small>
                </div>
                <p className="network-traffic-note">No public Silvus minimum per-node traffic floor is specified. This model uses payload rate plus configurable planning overhead; the default assumes one 3 kB message each second.</p>
              </section>
            </div>
          </details>

          <details className="network-control-card">
            <summary className="network-control-heading"><div className="network-control-title"><span>02</span><h2>Link assumptions</h2></div><p>Shared antenna and margin settings</p><div className="network-control-summary"><span>{inputs.antennaGainDbi} dBi</span><span>{inputs.crossPolarization}</span><span>{inputs.safetyMarginDb} dB margin</span></div><i aria-hidden="true" className="network-control-toggle" /></summary>
            <div className="network-control-body">
              <NetworkNumber id="network-antenna-gain" label="Antenna gain per node" max={35} min={0} onChange={(value) => updateInput("antennaGainDbi", value)} step={0.5} unit="dBi" value={inputs.antennaGainDbi} />
              <NetworkNumber id="network-cable-loss" label="Cable loss per node" max={10} min={0} onChange={(value) => updateInput("cableLossDb", value)} step={0.1} unit="dB" value={inputs.cableLossDb} />
              <NetworkNumber id="network-margin" label="Safety margin" max={30} min={0} onChange={(value) => updateInput("safetyMarginDb", value)} step={1} unit="dB" value={inputs.safetyMarginDb} />
              <NetworkSelect id="network-cross-polarization" label="Cross-polarized antennas" onChange={(value) => updateInput("crossPolarization", value as CrossPolarization)} options={["No", "One Side", "Both Sides"].map((value) => ({ label: value, value }))} value={inputs.crossPolarization} />
              <label className="network-check"><span><strong>Fresnel obstruction</strong><small>Apply a 6 dB penalty to every modeled path</small></span><input checked={inputs.fresnelBlocked} onChange={(event) => updateInput("fresnelBlocked", event.target.checked)} type="checkbox" /></label>
            </div>
          </details>

          <details className="network-control-card">
            <summary className="network-control-heading"><div className="network-control-title"><span>03</span><h2>Radio type groups</h2></div><p>Allocate a percentage of nodes to each radio profile</p><div className="network-control-summary"><span>{100 - sl5200Percent}% 4000</span><span>{sl5200Percent}% SL5200</span><span>{mixPlacement}</span></div><i aria-hidden="true" className="network-control-toggle" /></summary>
            <div className="network-control-body">
              <div className="network-radio-mix" aria-label="Target radio type distribution">
                <div><i className="network-swatch network-swatch--4000" /><span>4000 Series</span><strong>{100 - sl5200Percent}%</strong><small>{target4000Count} nodes</small></div>
                <div><i className="network-swatch network-swatch--5200" /><span>SL5200 estimated</span><strong>{sl5200Percent}%</strong><small>{targetSl5200Count} nodes</small></div>
              </div>
              <NetworkNumber id="sl5200-percentage" label="SL5200 share" max={100} min={0} onChange={setSl5200Percent} step={1} unit="%" value={sl5200Percent} />
              <input aria-label="SL5200 node percentage slider" className="network-range network-range--mix" max={100} min={0} onChange={(event) => setSl5200Percent(Number(event.target.value))} step={1} type="range" value={sl5200Percent} />
              <NetworkSelect id="radio-mix-placement" label="Group placement" onChange={(value) => setMixPlacement(value as RadioMixPlacement)} options={[{ label: "Grouped blocks", value: "Grouped blocks" }, { label: "Evenly distributed", value: "Evenly distributed" }]} value={mixPlacement} />
              <button className="network-apply-button" onClick={() => setNodes((current) => assignRadioMix(current, sl5200Percent, mixPlacement))} type="button">Apply radio mix</button>
              <p className="network-current-mix">Current inventory: <strong>{current4000Count} 4000 Series</strong> · <strong>{currentSl5200Count} SL5200</strong></p>
            </div>
          </details>

          <details className="network-control-card">
            <summary className="network-control-heading"><div className="network-control-title"><span>04</span><h2>Bulk node defaults</h2></div><p>Apply a common starting point, then edit nodes individually</p><div className="network-control-summary"><span>{RADIO_PROFILES[bulkProfile].shortLabel}</span><span>{bulkGroup}</span><span>{bulkAltitude} ft</span></div><i aria-hidden="true" className="network-control-toggle" /></summary>
            <div className="network-control-body">
              <NetworkSelect id="bulk-radio-profile" label="Radio profile" onChange={(value) => setBulkProfile(value as RadioProfileKey)} options={RADIO_KEYS.map((value) => ({ label: RADIO_PROFILES[value].label, value }))} value={bulkProfile} />
              <NetworkSelect id="bulk-height-group" label="Height group" onChange={(value) => { const group = value as HeightGroup; setBulkGroup(group); setBulkAltitude(HEIGHT_GROUP_DEFAULTS[group]); }} options={HEIGHT_GROUPS.map((value) => ({ label: value, value }))} value={bulkGroup} />
              <NetworkNumber id="bulk-altitude" label="Altitude / antenna height" max={MAX_TX_HEIGHT_FEET} min={1} onChange={setBulkAltitude} step={1} unit="ft AGL" value={bulkAltitude} />
              <button className="network-apply-button" onClick={() => setNodes((current) => current.map((node) => ({ ...node, radioProfile: bulkProfile, heightGroup: bulkGroup, altitudeFeet: bulkAltitude })))} type="button">Apply to all nodes</button>
            </div>
          </details>

          <button className="network-reset-button" onClick={resetNetwork} type="button">Reset network defaults</button>
        </aside>

        <div className="network-results">
          <div className="network-summary-strip">
            <div><span>Total offered traffic</span><strong>{results.totalTrafficMbps.toFixed(1)} Mbps</strong></div>
            <div><span>Reliable / possible</span><strong>{results.reliableLinks} / {results.possibleLinks}</strong></div>
            <div><span>Weakest modeled SNR</span><strong>{results.weakestSnrDb?.toFixed(1) ?? "—"} dB</strong></div>
            <div><span>Most-loaded radio airtime</span><strong>{Number.isFinite(results.maxNodeDutyCyclePercent) ? `${results.maxNodeDutyCyclePercent.toFixed(0)}% · node ${results.mostLoadedNodeId}` : ">100%"}</strong></div>
            <div><span>{inputs.topology === "Random relay chain" ? "Max viable branch" : "Max viable chain"}</span><strong>{results.maxPossibleChainLinks === null ? "Direct links" : `${results.maxPossibleChainLinks} / ${results.requestedChainLinks} hops${inputs.topology === "Random relay chain" ? ` · ${results.branchCount} branches` : ""}`}</strong></div>
          </div>

          <div className="network-propagation-grid" aria-label="Multi-node propagation calculations">
            <div><span>Environment model</span><strong>n = {results.environment.exponent.toFixed(1)}</strong><small>{inputs.environment}</small></div>
            <div><span>Average slant range</span><strong>{results.averageLinkDistanceKm.toFixed(2)} km</strong><small>Altitude-adjusted modeled hop</small></div>
            <div><span>Average-hop path loss</span><strong>{results.averagePathLossDb.toFixed(1)} dB</strong><small>{inputs.frequencyMHz} MHz · {inputs.environment}</small></div>
            <div><span>First Fresnel zone</span><strong>{results.averageFresnelRadiusMeters.toFixed(1)} m</strong><small>{results.usableFresnelRadiusMeters.toFixed(1)} m clear radius recommended</small></div>
          </div>

          <NetworkMap
            hasPendingChanges={mapHasPendingChanges}
            layoutSeed={appliedMap.layoutSeed}
            links={appliedMap.links}
            nodes={appliedMap.nodes}
            onApplyChanges={applyMapChanges}
            status={appliedMap.status}
            topology={appliedMap.topology}
          />

          <section className="network-table-card">
            <div className="network-table-heading">
              <div><span className="section-kicker">Per-node configuration</span><h2>Node inventory</h2><p>Edit the radio, altitude, and height group for every node.</p></div>
              <div className="network-pagination"><button disabled={safeNodePage === 0} onClick={() => setNodePage((page) => Math.max(0, page - 1))} type="button">Previous</button><span>{safeNodePage + 1} / {nodePageCount}</span><button disabled={safeNodePage >= nodePageCount - 1} onClick={() => setNodePage((page) => Math.min(nodePageCount - 1, page + 1))} type="button">Next</button></div>
            </div>
            <div className="mimo-table-wrap">
              <table className="network-table">
                <thead><tr><th>Node</th><th>Name</th><th>Radio</th><th>Altitude / height</th><th>Height group</th><th>Gateway status</th></tr></thead>
                <tbody>
                  {nodes.slice(safeNodePage * PAGE_SIZE, (safeNodePage + 1) * PAGE_SIZE).map((node) => {
                    const incident = linksByNode.get(node.id) ?? [];
                    const disconnected = !connectedNodeIds.has(node.id);
                    const degraded = incident.some((link) => link.status !== "Reliable");
                    const nodeStatus = disconnected ? "Isolated" : degraded ? "Degraded" : "Connected";
                    return (
                      <tr key={node.id}>
                        <td><strong>{String(node.id).padStart(3, "0")}</strong>{node.id === 1 ? <small>Hub</small> : null}</td>
                        <td><input aria-label={`Node ${node.id} name`} maxLength={28} onChange={(event) => updateNode(node.id, { name: event.target.value })} type="text" value={node.name} /></td>
                        <td><select aria-label={`Node ${node.id} radio profile`} onChange={(event) => updateNode(node.id, { radioProfile: event.target.value as RadioProfileKey })} value={node.radioProfile}>{RADIO_KEYS.map((key) => <option key={key} value={key}>{RADIO_PROFILES[key].label}</option>)}</select></td>
                        <td><div className="network-cell-number"><input aria-label={`Node ${node.id} altitude in feet AGL`} max={MAX_TX_HEIGHT_FEET} min={1} onChange={(event) => updateNode(node.id, { altitudeFeet: clamp(Number(event.target.value), 1, MAX_TX_HEIGHT_FEET) })} step={1} type="number" value={node.altitudeFeet} /><span>ft AGL</span></div></td>
                        <td><select aria-label={`Node ${node.id} height group`} onChange={(event) => updateNode(node.id, { heightGroup: event.target.value as HeightGroup })} value={node.heightGroup}>{HEIGHT_GROUPS.map((group) => <option key={group}>{group}</option>)}</select></td>
                        <td><span className={`network-node-state network-node-state--${nodeStatus.toLowerCase()}`}>{nodeStatus}</span></td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </section>

          <section className="network-table-card">
            <div className="network-table-heading">
              <div><span className="section-kicker">Weakest paths first</span><h2>Link diagnostics</h2><p>Bidirectional capacity includes radio type, slant range, horizon, traffic accumulation, and shared RF assumptions.</p></div>
              <div className="network-pagination"><button disabled={safeLinkPage === 0} onClick={() => setLinkPage((page) => Math.max(0, page - 1))} type="button">Previous</button><span>{safeLinkPage + 1} / {linkPageCount}</span><button disabled={safeLinkPage >= linkPageCount - 1} onClick={() => setLinkPage((page) => Math.min(linkPageCount - 1, page + 1))} type="button">Next</button></div>
            </div>
            <div className="mimo-table-wrap">
              <table className="network-table network-link-table">
                <thead><tr><th>Path</th><th>Radio pair</th><th>Slant range</th><th>Traffic load</th><th>Capacity</th><th>SNR</th><th>Mode</th><th>Horizon</th><th>Assessment</th></tr></thead>
                <tbody>
                  {rankedLinks.slice(safeLinkPage * PAGE_SIZE, (safeLinkPage + 1) * PAGE_SIZE).map((link) => {
                    const from = nodesById.get(link.from)!;
                    const to = nodesById.get(link.to)!;
                    return (
                      <tr className={`network-link-row--${link.status.toLowerCase()}`} key={link.id}>
                        <td><strong>{link.from} → {link.to}</strong></td>
                        <td>{RADIO_PROFILES[from.radioProfile].shortLabel} / {RADIO_PROFILES[to.radioProfile].shortLabel}</td>
                        <td>{link.distanceKm.toFixed(2)} <small>km</small></td>
                        <td>{link.requiredMbps.toFixed(1)} <small>Mbps</small></td>
                        <td>{link.capacityMbps?.toFixed(1) ?? "—"} <small>Mbps</small></td>
                        <td>{link.snrDb.toFixed(1)} <small>dB</small></td>
                        <td>{link.mcs === null ? "—" : `MCS ${link.mcs} · ${link.spatialStreams}SS`}</td>
                        <td>{link.horizonClear ? "Clear" : "Blocked"}</td>
                        <td><span className={`assessment assessment--${link.status === "Reliable" ? "good" : link.status === "Possible" ? "moderate" : "weak"}`}>{link.status}</span></td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </section>

          <details className="network-model-notes"><summary>Multi-node model notes</summary><p>Node 1 is the gateway. Direct topology gives each node one gateway path. Chain-link topology places every relay in one sequence. Random branches keep the gateway fixed, create several repeatable chains of unequal length, and distribute nodes between them using the displayed seed. Each upstream hop carries only the combined traffic of nodes downstream on its own branch. Gateway and relay load sums incident-link duty cycle to model shared half-duplex airtime; 80% is the reliable-planning limit and more than 100% is overloaded. Maximum viable branch length is the deepest continuous path whose hops clear horizon and carry their assigned traffic. Mixed-radio links use the weaker bidirectional result. As in the supplied workbook, 2SS requires cross-polarized antennas on both sides and incurs a 3 dB cross-polarization penalty. SL5200 results are estimates anchored to public 2 W, 2×2 MIMO, channel-bandwidth, sensitivity, and frequency-range specifications while reusing the compatible MN-MIMO curve. This planner is a conservative design aid, not a substitute for terrain analysis or field testing.</p></details>
        </div>
      </div>
    </section>
  );
}
