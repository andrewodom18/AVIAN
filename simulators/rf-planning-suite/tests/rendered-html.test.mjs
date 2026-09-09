import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import test from "node:test";

async function render(pathname = "/") {
  const workerUrl = new URL("../dist/server/index.js", import.meta.url);
  workerUrl.searchParams.set("test", `${process.pid}-${Date.now()}-${pathname}`);
  const { default: worker } = await import(workerUrl.href);

  return worker.fetch(
    new Request(`http://localhost${pathname}`, {
      headers: { accept: "text/html" },
    }),
    {
      ASSETS: {
        fetch: async () => new Response("Not found", { status: 404 }),
      },
    },
    {
      waitUntil() {},
      passThroughOnException() {},
    },
  );
}

test("server-renders the unified MN-MIMO RF planning suite", async () => {
  const response = await render("/");
  assert.equal(response.status, 200);
  assert.match(response.headers.get("content-type") ?? "", /^text\/html\b/i);

  const html = await response.text();
  assert.match(html, /<title>4000 Series \+ SL5200 RF Planning Suite<\/title>/i);
  assert.match(html, /<h1><span>4000 Series \+ SL5200<\/span> RF Planning Suite<\/h1>/);
  assert.doesNotMatch(html, /Unified workbook-derived model/);
  assert.match(html, /Scenario assessment/);
  assert.match(html, /class="mimo-status__recommendation"/);
  assert.doesNotMatch(html, /mimo-guidance-card/);
  assert.match(html, /Mode performance/);
  assert.match(html, /Radio horizon/);
  assert.match(html, /First Fresnel zone/);
  assert.match(html, /Modeled path loss/);
  assert.match(html, /Visual path map/);
  assert.match(html, /aria-label="Visual link-path map/);
  assert.match(html, /Environment-derived antenna profiles/);
  assert.match(html, /Directional Yagi/);
  assert.match(html, /Sector panel/);
  assert.match(html, /aria-label="Map legend"/);
  assert.match(html, /Reset link defaults/);
  assert.match(html, /Multi-node network/);
  assert.match(html, /Up to 150 mixed-radio nodes/);
  assert.equal((html.match(/<details class="mimo-input-card">/g) ?? []).length, 4);
  assert.equal((html.match(/<details class="mimo-input-card mimo-input-card--plain mimo-input-card--unnumbered">/g) ?? []).length, 1);
  assert.equal((html.match(/class="mimo-summary-pill /g) ?? []).length, 12);
  assert.match(html, /mimo-summary-pill--blue/);
  assert.match(html, /mimo-summary-pill--green/);
  assert.match(html, /mimo-summary-pill--amber/);
  assert.match(html, /Variable definitions/);
  assert.match(html, /Reference glossary/);
  assert.match(html, /Receiver-added noise/);
  assert.match(html, /aria-label="Quick radio-band presets"/);
  assert.match(html, /UHF/);
  assert.match(html, /L-Band/);
  assert.match(html, /S-Band/);
  assert.match(html, /C-Band/);
  assert.equal((html.match(/<details class="mimo-definition-group">/g) ?? []).length, 4);
  assert.doesNotMatch(html, /<details class="mimo-definition-group" open/);
  assert.doesNotMatch(html, /<details class="mimo-input-card" open/);
  assert.match(html, /value="2350"/);
  assert.match(html, /max="30000"/);
  assert.match(html, /29\.47/);
  assert.match(html, /QPSK 3\/4/);
  assert.doesNotMatch(html, /href="\/mimo"|Link Budget Visualizer|Interactive radio path profile/);
  assert.doesNotMatch(html, /codex-preview|Your site is taking shape|Building your site/);
});

test("removes the duplicate MN-MIMO route", async () => {
  const response = await render("/mimo");
  assert.equal(response.status, 404);
});

test("keeps only the finished single-page application source", async () => {
  const [page, layout, linkMap, networkPlanner, networkMap, model, packageJson] = await Promise.all([
    readFile(new URL("../app/page.tsx", import.meta.url), "utf8"),
    readFile(new URL("../app/layout.tsx", import.meta.url), "utf8"),
    readFile(new URL("../app/LinkMap.tsx", import.meta.url), "utf8"),
    readFile(new URL("../app/MultiNodePlanner.tsx", import.meta.url), "utf8"),
    readFile(new URL("../app/NetworkMap.tsx", import.meta.url), "utf8"),
    readFile(new URL("../app/model.ts", import.meta.url), "utf8"),
    readFile(new URL("../package.json", import.meta.url), "utf8"),
  ]);

  assert.match(page, /calculateMimo/);
  assert.match(page, /function InputCard/);
  assert.match(page, /function SummaryPill/);
  assert.match(page, /VARIABLE_DEFINITION_GROUPS/);
  assert.match(page, /<InputCard note="Reference glossary" title="Variable definitions">/);
  assert.doesNotMatch(page, /note="Reference glossary" number="05"/);
  assert.match(page, /<LinkMap/);
  assert.match(page, /<MultiNodePlanner/);
  assert.doesNotMatch(page, /href="\/mimo"|Link Budget Visualizer/);
  assert.match(linkMap, /ResizeObserver/);
  assert.match(linkMap, /mimo-map-canvas/);
  assert.match(linkMap, /mimo-map-card--invalid/);
  assert.match(linkMap, /status === "No viable mode"/);
  assert.match(linkMap, /ENVIRONMENT_SCENES/);
  assert.match(linkMap, /drawLatticeTower/);
  assert.match(linkMap, /drawAircraft/);
  assert.match(linkMap, /environment === "Air to Air"/);
  assert.match(linkMap, /drawAircraft\(context, right \+ 9, rxTop - 17, "#2dd4bf", -1\)/);
  assert.doesNotMatch(`${page}\n${linkMap}`, /U-28|PC-12|Draco/);
  assert.match(linkMap, /drawAircraft\(context, left - 9, txTop - 17/);
  assert.match(linkMap, /drawVentralBladeAntenna\(context, left, txTop/);
  assert.match(linkMap, /Math\.log1p\(boundedAltitude\)/);
  assert.match(linkMap, /drawShip/);
  assert.match(linkMap, /drawRover/);
  assert.match(model, /DEFAULT_MIMO_INPUTS/);
  assert.match(model, /RADIO_PROFILES/);
  assert.match(model, /SL5200 \(estimated\)/);
  assert.match(model, /calculateNetwork/);
  assert.match(model, /createNetworkNodes/);
  assert.match(networkPlanner, /max=\{150\}/);
  assert.match(networkPlanner, /Direct to hub/);
  assert.match(networkPlanner, /Relay chain/);
  assert.match(networkPlanner, /Chain-link mode/);
  assert.match(networkPlanner, /Average distance between nodes/);
  assert.match(networkPlanner, /network-geometry-bar/);
  assert.match(networkPlanner, /Typical message size/);
  assert.match(networkPlanner, /Messages per second/);
  assert.match(networkPlanner, /Planning overhead/);
  assert.match(networkPlanner, /Extensive \+5\.5 Mbps/);
  assert.match(networkPlanner, /No public Silvus minimum per-node traffic floor/);
  assert.match(networkPlanner, /calculatePacketTrafficMbps/);
  assert.match(networkPlanner, /Node inventory/);
  assert.match(networkPlanner, /Link diagnostics/);
  assert.match(networkPlanner, /Multi-node radio-band presets/);
  assert.match(networkPlanner, /Average-hop path loss/);
  assert.match(networkPlanner, /First Fresnel zone/);
  assert.match(networkPlanner, /Radio type groups/);
  assert.match(networkPlanner, /SL5200 node percentage slider/);
  assert.match(networkPlanner, /Apply radio mix/);
  assert.match(networkPlanner, /Evenly distributed/);
  assert.match(networkPlanner, /Random relay chain/);
  assert.match(networkPlanner, /Random branches/);
  assert.match(networkPlanner, /Current branch paths/);
  assert.match(networkPlanner, /results\.branchCount/);
  assert.match(networkPlanner, /Reshuffle branches/);
  assert.match(networkPlanner, /Max viable chain/);
  assert.equal((networkPlanner.match(/<details className="network-control-card">/g) ?? []).length, 4);
  assert.doesNotMatch(networkPlanner, /<details className="network-control-card" open/);
  assert.match(networkPlanner, /network-control-summary/);
  assert.match(networkMap, /ResizeObserver/);
  assert.match(networkMap, /network-map-canvas/);
  assert.match(networkMap, /perspectiveProject/);
  assert.match(networkMap, /topologyLayoutHash/);
  assert.match(networkMap, /seededLayoutRandom/);
  assert.match(networkMap, /layoutRotation/);
  assert.match(networkMap, /topologyLayoutHash\(links\) \^ layoutSeed/);
  assert.match(networkMap, /function linkDegradationPoint/);
  assert.match(networkMap, /function addLinkHealthStops/);
  assert.match(networkMap, /gradient\.addColorStop\(degradationPoint, yellow\)/);
  assert.match(networkMap, /gradient\.addColorStop\(Math\.min\(0\.96, degradationPoint \+ 0\.18\), red\)/);
  assert.match(networkMap, /Strong throughout/);
  assert.match(networkMap, /Weakens near node/);
  assert.match(networkMap, /Degrades to failed/);
  assert.match(networkMap, /node\.radioProfile === "series4000"/);
  assert.match(networkMap, /index < 6/);
  assert.match(networkMap, /Apply changes/);
  assert.match(networkMap, /Map up to date/);
  assert.match(networkMap, /4000 Series · diamond/);
  assert.match(networkMap, /SL5200 estimated · hexagon/);
  assert.match(networkPlanner, /draftMapSignature/);
  assert.match(networkPlanner, /appliedMapSignature/);
  assert.match(networkPlanner, /const applyMapChanges/);
  assert.match(networkPlanner, /hasPendingChanges=\{mapHasPendingChanges\}/);
  assert.match(networkPlanner, /layoutSeed=\{appliedMap\.layoutSeed\}/);
  assert.match(networkPlanner, /onApplyChanges=\{applyMapChanges\}/);
  assert.match(networkMap, /Interactive 3D topology/);
  assert.match(networkMap, /3D multi-node network map/);
  assert.match(networkMap, /onPointerMove/);
  assert.match(networkMap, /addEventListener\("wheel", handleWheel, \{ passive: false \}\)/);
  assert.match(networkMap, /MAX_CAMERA_ZOOM = 5/);
  assert.match(networkMap, /Math\.exp\(-event\.deltaY \* \.0015\)/);
  assert.match(networkMap, /panX: pointerX - centerX/);
  assert.match(networkMap, /Map zoom level/);
  assert.match(networkMap, /event\.preventDefault\(\)/);
  assert.match(networkMap, /event\.stopPropagation\(\)/);
  assert.doesNotMatch(networkMap, /onWheel=\{onWheel\}/);
  assert.match(networkMap, /Drag to orbit/);
  assert.match(networkMap, /Altitude stems use a logarithmic scale/);
  assert.match(model, /ENVIRONMENTS/);
  assert.match(model, /MAX_TX_HEIGHT_FEET = 30_000/);
  assert.match(model, /AIR_TO_AIR_DEFAULT_ALTITUDE_FEET = 10_000/);
  assert.match(model, /"Air to Air": \{ exponent: 2/);
  assert.match(page, /applyEnvironment/);
  assert.match(networkPlanner, /applyEnvironmentDefaultsToNodes/);
  assert.match(page, /max=\{MAX_TX_HEIGHT_FEET\}/);
  assert.match(layout, /4000 Series \+ SL5200 RF Planning Suite/);
  assert.doesNotMatch(packageJson, /react-loading-skeleton/);
  assert.doesNotMatch(`${page}\n${layout}`, /codex-preview|_sites-preview|SkeletonPreview/);
  await assert.rejects(access(new URL("../app/mimo", import.meta.url)));
  await assert.rejects(access(new URL("../app/_sites-preview", import.meta.url)));
});
