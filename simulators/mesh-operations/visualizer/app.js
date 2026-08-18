import { validateRadioConfiguration } from "./radio-config.mjs";

const svgNamespace = "http://www.w3.org/2000/svg";
const state = { scenario: null, stepIndex: 0, selectedNodeId: null, timer: null, playing: false };

const elements = Object.fromEntries([
  "traceVersion", "stepTitle", "stepNarrative", "onlineNodes", "activeLinksLabel", "activeLinks", "components",
  "missionSync", "continuity", "topology", "nodeLabel", "nodeStatus", "nodeDetails",
  "commandState", "integrityBar", "operatorReadout", "timeline", "playButton", "stepButton",
  "resetButton", "speedSelect", "errorBanner", "controlActivity", "controlRequest", "controlStatus",
  "simulationTab", "aboutTab", "simulationView", "aboutView", "guidedFocus", "guidedFocusTitle", "guidedFocusText",
  "radioConfigPanel", "radioConfigForm", "networkIdInput", "bandInput", "frequencyInput", "bandwidthInput",
  "powerInput", "beaconInput", "encryptionInput", "validateRadioButton", "applyRadioButton", "radioConfigResult",
].map((id) => [id, document.getElementById(id)]));

const guidedSteps = {
  "radio-connected": ["Management connection detected", "Watch the local radio appear while CHUD uses GET /api/radio/devices to refresh its inventory."],
  "radios-discovered": ["Four candidate radios identified", "The cyan node markers are discovered identities. No mesh links are assumed at this point."],
  "radio-snapshot": ["Safe pre-change snapshot", "CHUD reads current settings before making changes so the transaction has a known recovery point."],
  "radio-config-applied": ["Validated configuration applied", "The highlighted CHUD control shows POST /api/radio/apply and its simulated response."],
  "radio-config-confirmed": ["Readback confirms the change", "CHUD verifies effective settings before releasing the radios for AVIAN formation startup."],
};

function svgElement(name, attributes = {}) {
  const element = document.createElementNS(svgNamespace, name);
  for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, String(value));
  return element;
}

function nodeById(step, id) { return step.nodes.find((node) => node.id === id); }
function position(step, id) {
  const node = nodeById(step, id);
  return node ? { x: node.x, y: node.y } : null;
}

function renderFormationSummary(step) {
  const summary = step.formation_summary;
  const center = { x: 50, y: 44 };
  const aircraftNodes = step.nodes.filter((node) => node.role === "aircraft");
  const groundNode = step.nodes.find((node) => node.role === "ground");
  const positions = new Map();

  aircraftNodes.forEach((node, index) => {
    const ring = Math.floor(index / 40);
    const slot = index % 40;
    const radiusX = 10 + ring * 6;
    const radiusY = 8 + ring * 5;
    const angle = (slot / 40) * Math.PI * 2 + (ring % 2 ? Math.PI / 40 : 0);
    positions.set(node.id, {
      x: center.x + Math.cos(angle) * radiusX,
      y: center.y + Math.sin(angle) * radiusY,
    });
  });
  if (groundNode) positions.set(groundNode.id, center);

  const title = svgElement("text", { x: 50, y: 5.5, class: "formation-title" });
  title.textContent = `${summary.simulated_aircraft.toLocaleString()} SIMULATED AIRCRAFT + ${summary.control_stations} CONTROL STATION`;
  const subtitle = svgElement("text", { x: 50, y: 9.2, class: "formation-subtitle" });
  subtitle.textContent = step.id === "maximum-formation-rerouting"
    ? "DISTRIBUTED NODE LOSS · ACTIVE PATHS CHANGE · SURVIVORS REMAIN CONNECTED"
    : step.id === "maximum-formation-online"
      ? "ALL 200 AIRCRAFT ONLINE · LIVE SPIDER-WEB PATHS · RF VALIDATION PENDING"
      : "ALL 200 AIRCRAFT RECOVERED · PATHS RESTORED · RF VALIDATION PENDING";
  elements.topology.append(title, subtitle);

  const networkLayer = svgElement("g", { class: "scale-network scale-zoom-out" });

  for (const link of step.links) {
    const from = positions.get(link.source);
    const to = positions.get(link.target);
    if (!from || !to) continue;
    networkLayer.append(svgElement("path", {
      d: `M ${from.x} ${from.y} L ${to.x} ${to.y}`,
      class: `scale-link ${link.state}`,
    }));
  }

  aircraftNodes.forEach((node, index) => {
    const point = positions.get(node.id);
    const aircraft = svgElement("g", {
      class: `scale-aircraft ${node.status}`,
      transform: `translate(${point.x} ${point.y})`,
      style: `--node-delay:${Math.min(index * 4, 700)}ms`,
    });
    aircraft.append(svgElement("circle", { r: .72, class: "scale-aircraft-halo" }));
    aircraft.append(svgElement("circle", { r: .38, class: "scale-aircraft-core" }));
    const tooltip = svgElement("title");
    tooltip.textContent = `${node.id} · ${node.flight_stack} · ${node.status} · ${node.mission_synced ? "mission synchronized" : "awaiting synchronization"}`;
    aircraft.append(tooltip);
    networkLayer.append(aircraft);
  });

  const control = svgElement("g", { class: `formation-control scale-control ${groundNode?.status ?? "offline"}`, transform: `translate(${center.x} ${center.y})` });
  control.append(svgElement("circle", { r: 4.8, class: "control-halo" }));
  control.append(svgElement("rect", { x: -4.6, y: -3.1, width: 9.2, height: 6.2, rx: 1.1, class: "control-core" }));
  const controlGlyph = svgElement("text", { x: 0, y: .7, class: "control-glyph" });
  controlGlyph.textContent = "GCS 01";
  const controlLabel = svgElement("text", { x: 0, y: 5.9, class: "cluster-label" });
  controlLabel.textContent = "UNIVERSAL COCKPIT";
  control.append(controlGlyph, controlLabel);
  networkLayer.append(control);
  elements.topology.append(networkLayer);

  const note = svgElement("text", { x: 50, y: 79, class: "formation-note" });
  const offlineAircraft = aircraftNodes.filter((node) => node.status === "offline").length;
  note.textContent = offlineAircraft
    ? `${step.metrics.online_nodes} nodes online · ${offlineAircraft} aircraft offline · surviving mesh paths rerouted`
    : `${aircraftNodes.length} simulated aircraft nodes + 1 GCS · mission state synchronized`;
  elements.topology.append(note);
}

function renderGuidedFocus(step) {
  const guidance = guidedSteps[step.id];
  const topologyCard = elements.topology.closest(".topology-card");
  topologyCard.classList.toggle("guided-target", Boolean(guidance));
  elements.controlActivity.classList.toggle("guided-control", Boolean(guidance && step.control_event));
  elements.radioConfigPanel.classList.toggle("guided-radio-control", ["radio-snapshot", "radio-config-applied", "radio-config-confirmed"].includes(step.id));
  elements.guidedFocus.hidden = !guidance;
  if (!guidance) return;
  elements.guidedFocusTitle.textContent = guidance[0];
  elements.guidedFocusText.textContent = guidance[1];
}

function renderTopology(step) {
  elements.topology.replaceChildren();
  if (step.formation_summary) {
    renderFormationSummary(step);
    return;
  }
  const defs = svgElement("defs");
  const glow = svgElement("filter", { id: "packet-glow", x: "-200%", y: "-200%", width: "500%", height: "500%" });
  glow.append(svgElement("feGaussianBlur", { stdDeviation: "0.7", result: "blur" }));
  const merge = svgElement("feMerge");
  merge.append(svgElement("feMergeNode", { in: "blur" }), svgElement("feMergeNode", { in: "SourceGraphic" }));
  glow.append(merge); defs.append(glow); elements.topology.append(defs);

  for (const [index, link] of step.links.entries()) {
    const from = position(step, link.source);
    const to = position(step, link.target);
    if (!from || !to) continue;
    const pathId = `link-${index}`;
    const midpoint = { x: (from.x + to.x) / 2, y: (from.y + to.y) / 2 };
    const pathData = link.state === "failover"
      ? `M ${from.x} ${from.y} Q ${midpoint.x + 4} ${midpoint.y - 7} ${to.x} ${to.y}`
      : `M ${from.x} ${from.y} L ${to.x} ${to.y}`;
    const path = svgElement("path", {
      id: pathId,
      d: pathData,
      class: `mesh-link ${link.state}`,
    });
    elements.topology.append(path);
    const label = svgElement("text", {
      x: midpoint.x + (link.state === "failover" ? 3 : 0),
      y: midpoint.y - (link.state === "failover" ? 5 : 1.6),
      class: "link-label",
    });
    label.textContent = `${link.transport} · ${link.latency_ms}ms`;
    elements.topology.append(label);
    if (["active", "failover"].includes(link.state)) {
      const packet = svgElement("circle", {
        r: link.state === "failover" ? 0.72 : 0.52,
        fill: link.state === "failover" ? "#7d9fff" : "#9affcd",
        class: "packet",
        filter: "url(#packet-glow)",
      });
      const motion = svgElement("animateMotion", {
        dur: link.state === "failover" ? "1.05s" : `${1.25 + index * .11}s`,
        repeatCount: "indefinite",
        path: pathData,
      });
      packet.append(motion); elements.topology.append(packet);
    }
  }

  for (const node of step.nodes) {
    const group = svgElement("g", {
      class: `node ${node.role} ${node.status}${state.selectedNodeId === node.id ? " selected" : ""}`,
      transform: `translate(${node.x} ${node.y})`,
      tabindex: "0",
      role: "button",
      "aria-label": `${node.label}, ${node.status}`,
    });
    group.append(svgElement("circle", { r: 6.4, class: "halo" }));
    if (node.mission_synced) group.append(svgElement("circle", { r: 5.35, class: "mission-ring" }));
    const core = node.role === "ground"
      ? svgElement("rect", { x: -3, y: -3, width: 6, height: 6, rx: 1.1, class: "core" })
      : svgElement("polygon", { points: "0,-3.7 3.25,-1.85 3.25,1.85 0,3.7 -3.25,1.85 -3.25,-1.85", class: "core" });
    group.append(core);
    const glyph = svgElement("text", { x: 0, y: .8, class: "node-label" });
    glyph.textContent = node.role === "ground" ? "G" : "A";
    group.append(glyph);
    const label = svgElement("text", { x: 0, y: 8.2, class: "node-label" });
    label.textContent = node.label;
    const sub = svgElement("text", { x: 0, y: 10.8, class: "node-sub" });
    sub.textContent = node.flight_stack ?? "GROUND PEER";
    group.append(label, sub);
    const select = () => { state.selectedNodeId = node.id; render(); };
    group.addEventListener("click", select);
    group.addEventListener("keydown", (event) => { if (["Enter", " "].includes(event.key)) select(); });
    elements.topology.append(group);
  }
}

function detailRows(step, node) {
  if (step.formation_summary) {
    const summary = step.formation_summary;
    return [
      ["Mission", summary.mission_id],
      ["Simulated aircraft", summary.simulated_aircraft.toLocaleString()],
      ["Control stations", summary.control_stations],
      ["Ground partition", summary.ground_partition_continuity_verified ? "continuity verified" : "not yet injected"],
      ["Distributed loss", summary.distributed_loss_nodes ? `${summary.distributed_loss_nodes} removed; continuity verified` : "not yet injected"],
      ["Recovery", summary.recovery_converged ? "201 nodes converged" : "pending"],
      ["Validation", summary.field_validated ? "field validated" : "simulation only"],
    ];
  }
  if (!node) {
    return [
      ["Formation", state.scenario.name],
      ["Current phase", step.phase],
      ["Scenario step", `${state.stepIndex + 1} / ${state.scenario.steps.length}`],
      ["Connected components", step.metrics.connected_components],
      ["Continuity", step.metrics.continuity],
      ["Trace time", `${(step.at_ms / 1000).toFixed(1)} s`],
    ];
  }
  const peerLinks = step.links.filter((link) => link.source === node.id || link.target === node.id);
  return [
    ["Node ID", node.id],
    ["Role", node.role],
    ["Flight stack", node.flight_stack ?? "N/A"],
    ["Mission state", node.mission_synced ? "generation 1 synced" : "not present"],
    ["Visible records", node.record_count],
    ["Peer paths", peerLinks.filter((link) => ["active", "failover"].includes(link.state)).length],
  ];
}

function renderDetails(step) {
  const node = nodeById(step, state.selectedNodeId);
  elements.nodeLabel.textContent = step.formation_summary ? "Large-formation summary" : node?.label ?? "Formation overview";
  elements.nodeStatus.textContent = (node?.status ?? "ready").toUpperCase();
  elements.nodeStatus.className = `status-chip ${node?.status ?? "neutral"}`;
  elements.nodeDetails.replaceChildren(...detailRows(step, node).map(([label, value]) => {
    const row = document.createElement("div");
    const term = document.createElement("dt"); term.textContent = label;
    const description = document.createElement("dd"); description.textContent = String(value);
    row.append(term, description); return row;
  }));
  const verified = step.metrics.signed_command_verified;
  const control = step.control_event;
  elements.controlActivity.hidden = !control;
  elements.controlRequest.textContent = control ? `${control.method} ${control.path}` : "";
  elements.controlStatus.textContent = control ? control.status : "";
  elements.commandState.textContent = verified ? "VERIFIED + ACKNOWLEDGED" : "GUARDS ACTIVE";
  elements.commandState.style.color = verified ? "#52f0a5" : "#d2e2ea";
  elements.integrityBar.style.width = verified ? "100%" : `${22 + state.stepIndex * 6}%`;
  elements.integrityBar.style.background = verified ? "#52f0a5" : "#58707d";
  elements.operatorReadout.textContent = node
    ? `${node.label} is ${node.status}. ${node.mission_synced ? "Its durable mission record is current." : "It has not received the mission record."}`
    : step.narrative;
}

function renderTimeline() {
  elements.timeline.replaceChildren(...state.scenario.steps.map((step, index) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `timeline-step${index < state.stepIndex ? " complete" : ""}${index === state.stepIndex ? " current" : ""}`;
    button.innerHTML = `<strong>${step.title}</strong><small>T+${(step.at_ms / 1000).toFixed(1)} SEC</small>`;
    button.addEventListener("click", () => { state.stepIndex = index; stop(); render(); });
    return button;
  }));
}

function render() {
  const step = state.scenario.steps[state.stepIndex];
  if (state.selectedNodeId && !nodeById(step, state.selectedNodeId)) state.selectedNodeId = null;
  elements.traceVersion.textContent = `SCHEMA ${state.scenario.schema_version} · STEP ${state.stepIndex + 1}/${state.scenario.steps.length}`;
  elements.stepTitle.textContent = step.title;
  elements.stepNarrative.textContent = step.narrative;
  elements.onlineNodes.textContent = step.metrics.online_nodes;
  elements.activeLinksLabel.textContent = step.formation_summary ? "AIRCRAFT" : "LINKS";
  elements.activeLinks.textContent = step.formation_summary ? step.formation_summary.simulated_aircraft : step.metrics.active_links;
  elements.activeLinks.nextElementSibling.textContent = step.formation_summary ? "simulated" : "active";
  elements.components.textContent = step.metrics.connected_components;
  const formationSize = step.formation_summary
    ? step.formation_summary.simulated_aircraft + step.formation_summary.control_stations
    : step.nodes.length;
  elements.missionSync.textContent = `${step.metrics.mission_synced_nodes}/${formationSize}`;
  elements.missionSync.nextElementSibling.hidden = Boolean(step.formation_summary);
  elements.continuity.textContent = step.metrics.continuity.toUpperCase();
  renderTopology(step); renderDetails(step); renderTimeline(); renderGuidedFocus(step);
}

function stop() {
  clearInterval(state.timer); state.timer = null; state.playing = false;
  elements.playButton.textContent = "▶ PLAY";
}
function play() {
  if (state.playing) { stop(); return; }
  state.playing = true; elements.playButton.textContent = "Ⅱ PAUSE";
  const advance = () => {
    if (state.stepIndex >= state.scenario.steps.length - 1) { stop(); return; }
    state.stepIndex += 1; render();
    scheduleNext();
  };
  const scheduleNext = () => {
    const guidedDelay = state.stepIndex < 5
      ? 4400
      : state.stepIndex >= state.scenario.steps.length - 3
        ? 3200
        : state.stepIndex < 8 ? 2600 : 1900;
    state.timer = setTimeout(advance, guidedDelay / Number(elements.speedSelect.value));
  };
  scheduleNext();
}
function stepForward() { stop(); state.stepIndex = (state.stepIndex + 1) % state.scenario.steps.length; render(); }
function reset() { stop(); state.stepIndex = 0; state.selectedNodeId = null; render(); }

function radioFormValue() {
  return {
    network_id: elements.networkIdInput.value,
    band: elements.bandInput.value,
    center_frequency_mhz: elements.frequencyInput.value,
    bandwidth_mhz: elements.bandwidthInput.value,
    transmit_power_dbm: elements.powerInput.value,
    routing_beacon_period_ms: elements.beaconInput.value,
    encryption_required: elements.encryptionInput.checked,
  };
}

function populateRadioForm(config) {
  elements.networkIdInput.value = config.network_id;
  elements.bandInput.value = config.band;
  const [minimum, maximum] = bandDefaults[config.band];
  elements.frequencyInput.min = String(minimum);
  elements.frequencyInput.max = String(maximum);
  elements.frequencyInput.value = String(config.center_frequency_mhz);
  elements.bandwidthInput.value = String(config.bandwidth_mhz);
  elements.powerInput.value = String(config.transmit_power_dbm);
  elements.beaconInput.value = String(config.routing_beacon_period_ms);
  elements.encryptionInput.checked = config.encryption_required;
}

function showRadioResult(kind, title, message) {
  elements.radioConfigResult.className = `radio-config-result ${kind}`;
  elements.radioConfigResult.replaceChildren();
  const strong = document.createElement("strong"); strong.textContent = title;
  const detail = document.createElement("span"); detail.textContent = message;
  elements.radioConfigResult.append(strong, detail);
}

function validateRadioForm() {
  try {
    const config = validateRadioConfiguration(radioFormValue());
    showRadioResult(
      "success",
      "PLAN VALID",
      `${config.band} · ${config.center_frequency_mhz} MHz · ${config.bandwidth_mhz} MHz channel · ${config.transmit_power_dbm} dBm/port`,
    );
    return config;
  } catch (error) {
    showRadioResult("error", "VALIDATION BLOCKED", error instanceof Error ? error.message : String(error));
    return null;
  }
}

const bandDefaults = {
  "UHF-LOW": [225, 450, 350],
  UHF: [698, 970, 900],
  "L-BAND": [1250, 1850, 1625],
  "S-BAND": [1850, 2600, 2440],
  "C-BAND": [3200, 6000, 5200],
};

elements.bandInput.addEventListener("change", () => {
  const [minimum, maximum, fallback] = bandDefaults[elements.bandInput.value];
  elements.frequencyInput.min = String(minimum);
  elements.frequencyInput.max = String(maximum);
  const frequency = Number(elements.frequencyInput.value);
  if (frequency < minimum || frequency > maximum) elements.frequencyInput.value = String(fallback);
  validateRadioForm();
});

elements.validateRadioButton.addEventListener("click", validateRadioForm);
elements.radioConfigForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const config = validateRadioForm();
  if (!config) return;
  elements.applyRadioButton.disabled = true;
  showRadioResult("", "CHUD APPLYING", "Writing to the simulator only, then reading the effective configuration back.");
  try {
    const response = await fetch("/api/radio/configuration", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(config),
    });
    const result = await response.json();
    if (!response.ok) throw new Error(result.error ?? `Simulator returned ${response.status}`);
    showRadioResult(
      "success",
      result.status,
      `Generation ${result.configuration.generation} read back · ${result.configuration.center_frequency_mhz} MHz · hardware write: blocked`,
    );
  } catch (error) {
    showRadioResult("error", "APPLY FAILED", error instanceof Error ? error.message : String(error));
  } finally {
    elements.applyRadioButton.disabled = false;
  }
});

elements.playButton.addEventListener("click", play);
elements.stepButton.addEventListener("click", stepForward);
elements.resetButton.addEventListener("click", reset);
elements.speedSelect.addEventListener("change", () => { if (state.playing) { stop(); play(); } });
function showView(view) {
  const simulationVisible = view === "simulation";
  elements.simulationView.hidden = !simulationVisible;
  elements.aboutView.hidden = simulationVisible;
  elements.simulationTab.classList.toggle("active", simulationVisible);
  elements.aboutTab.classList.toggle("active", !simulationVisible);
  elements.simulationTab.setAttribute("aria-selected", String(simulationVisible));
  elements.aboutTab.setAttribute("aria-selected", String(!simulationVisible));
  if (!simulationVisible) stop();
}
elements.simulationTab.addEventListener("click", () => showView("simulation"));
elements.aboutTab.addEventListener("click", () => showView("about"));
window.addEventListener("keydown", (event) => {
  if (event.code === "Space" && event.target === document.body) { event.preventDefault(); play(); }
  if (event.code === "ArrowRight") stepForward();
  if (event.code === "Home") reset();
});

try {
  const response = await fetch("/api/scenario", { cache: "no-store" });
  if (!response.ok) throw new Error(`Simulator API returned ${response.status}`);
  state.scenario = await response.json();
  render();
  const radioResponse = await fetch("/api/radio/configuration", { cache: "no-store" });
  if (radioResponse.ok) {
    const radioState = await radioResponse.json();
    populateRadioForm(radioState.configuration);
    showRadioResult(
      "success",
      `GENERATION ${radioState.configuration.generation} LOADED`,
      `${radioState.configuration.band} · ${radioState.configuration.center_frequency_mhz} MHz · hardware write: blocked`,
    );
  }
} catch (error) {
  elements.errorBanner.hidden = false;
  elements.errorBanner.textContent = error instanceof Error ? error.message : String(error);
}
