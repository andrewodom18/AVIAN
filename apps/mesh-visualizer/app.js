const svgNamespace = "http://www.w3.org/2000/svg";
const state = { scenario: null, stepIndex: 0, selectedNodeId: null, timer: null, playing: false };

const elements = Object.fromEntries([
  "traceVersion", "stepTitle", "stepNarrative", "onlineNodes", "activeLinks", "components",
  "missionSync", "continuity", "topology", "nodeLabel", "nodeStatus", "nodeDetails",
  "commandState", "integrityBar", "operatorReadout", "timeline", "playButton", "stepButton",
  "resetButton", "speedSelect", "errorBanner",
].map((id) => [id, document.getElementById(id)]));

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

function renderTopology(step) {
  elements.topology.replaceChildren();
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
  if (!node) {
    return [
      ["Formation", state.scenario.name],
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
  elements.nodeLabel.textContent = node?.label ?? "Formation overview";
  elements.nodeStatus.textContent = (node?.status ?? "ready").toUpperCase();
  elements.nodeStatus.className = `status-chip ${node?.status ?? "neutral"}`;
  elements.nodeDetails.replaceChildren(...detailRows(step, node).map(([label, value]) => {
    const row = document.createElement("div");
    const term = document.createElement("dt"); term.textContent = label;
    const description = document.createElement("dd"); description.textContent = String(value);
    row.append(term, description); return row;
  }));
  const verified = step.metrics.signed_command_verified;
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
  elements.activeLinks.textContent = step.metrics.active_links;
  elements.components.textContent = step.metrics.connected_components;
  elements.missionSync.textContent = `${step.metrics.mission_synced_nodes}/${step.nodes.length}`;
  elements.continuity.textContent = step.metrics.continuity.toUpperCase();
  renderTopology(step); renderDetails(step); renderTimeline();
}

function stop() {
  clearInterval(state.timer); state.timer = null; state.playing = false;
  elements.playButton.textContent = "▶ PLAY";
}
function play() {
  if (state.playing) { stop(); return; }
  state.playing = true; elements.playButton.textContent = "Ⅱ PAUSE";
  state.timer = setInterval(() => {
    if (state.stepIndex >= state.scenario.steps.length - 1) { stop(); return; }
    state.stepIndex += 1; render();
  }, Number(elements.speedSelect.value));
}
function stepForward() { stop(); state.stepIndex = (state.stepIndex + 1) % state.scenario.steps.length; render(); }
function reset() { stop(); state.stepIndex = 0; state.selectedNodeId = null; render(); }

elements.playButton.addEventListener("click", play);
elements.stepButton.addEventListener("click", stepForward);
elements.resetButton.addEventListener("click", reset);
elements.speedSelect.addEventListener("change", () => { if (state.playing) { stop(); play(); } });
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
} catch (error) {
  elements.errorBanner.hidden = false;
  elements.errorBanner.textContent = error instanceof Error ? error.message : String(error);
}
