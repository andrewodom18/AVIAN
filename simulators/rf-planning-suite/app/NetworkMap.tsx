"use client";

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import type { NetworkLinkResult, NetworkNode, NetworkTopology } from "./model";

type WorldPoint = { x: number; y: number; z: number };
type ScreenPoint = { x: number; y: number; depth: number };
type Camera = { yaw: number; pitch: number; zoom: number; panX: number; panY: number };

const MIN_CAMERA_ZOOM = 0.55;
const MAX_CAMERA_ZOOM = 5;
const DEFAULT_CAMERA: Camera = { yaw: -0.62, pitch: 0.7, zoom: 1, panX: 0, panY: 0 };

function clamp(value: number, minimum: number, maximum: number) {
  return Math.min(maximum, Math.max(minimum, value));
}

function altitudeToWorld(altitudeFeet: number) {
  return 0.05 + (Math.log1p(clamp(altitudeFeet, 1, 30000)) / Math.log1p(30000)) * 1.02;
}

function chainOrder(nodes: NetworkNode[], links: NetworkLinkResult[]) {
  if (!links.length) return nodes.map((node) => node.id);
  const ordered = [links[0].from, ...links.map((link) => link.to)];
  const seen = new Set(ordered);
  return [...ordered, ...nodes.filter((node) => !seen.has(node.id)).map((node) => node.id)];
}

function randomBranchPaths(nodes: NetworkNode[], links: NetworkLinkResult[]) {
  const gatewayId = nodes[0]?.id;
  if (gatewayId === undefined) return [] as number[][];
  const children = new Map<number, number[]>();
  for (const link of links) children.set(link.from, [...(children.get(link.from) ?? []), link.to]);
  return (children.get(gatewayId) ?? []).map((rootId) => {
    const path = [gatewayId];
    const seen = new Set(path);
    let current: number | undefined = rootId;
    while (current !== undefined && !seen.has(current)) {
      path.push(current);
      seen.add(current);
      current = children.get(current)?.[0];
    }
    return path;
  });
}

function topologyLayoutHash(links: NetworkLinkResult[]) {
  let hash = 2166136261;
  for (const link of links) {
    for (const character of `${link.from}>${link.to}|`) {
      hash ^= character.charCodeAt(0);
      hash = Math.imul(hash, 16777619);
    }
  }
  return hash >>> 0;
}

function seededLayoutRandom(seed: number) {
  let state = seed || 1;
  return () => {
    state += 0x6d2b79f5;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}

function topologyLabel(topology: NetworkTopology) {
  if (topology === "Relay chain") return "Chain link";
  if (topology === "Random relay chain") return "Random branches";
  return "Direct links";
}

function linkDegradationPoint(link: NetworkLinkResult) {
  if (link.status === "Reliable") return 1;
  if (link.status === "Possible") {
    const snrReserve = clamp(link.snrDb / 5, 0, 1);
    const capacityReserve = link.capacityMbps === null || link.requiredMbps <= 0
      ? 0
      : clamp((link.capacityMbps / link.requiredMbps - 1) / 0.25, 0, 1);
    return 0.42 + Math.min(snrReserve, capacityReserve) * 0.3;
  }

  const snrReach = clamp((link.snrDb + 10) / 10, 0.36, 0.82);
  const capacityReach = link.capacityMbps === null
    ? 0.36
    : link.requiredMbps <= 0
      ? 0.82
      : clamp(link.capacityMbps / link.requiredMbps, 0.36, 0.82);
  const horizonReach = link.horizonClear ? 0.82 : 0.55;
  return Math.min(snrReach, capacityReach, horizonReach);
}

function addLinkHealthStops(gradient: CanvasGradient, link: NetworkLinkResult) {
  const green = "rgba(45, 212, 191, .92)";
  const brightGreen = "rgba(74, 222, 128, .96)";
  const yellow = "rgba(251, 191, 36, .96)";
  const red = "rgba(251, 113, 133, .98)";
  const degradationPoint = linkDegradationPoint(link);

  gradient.addColorStop(0, green);
  if (link.status === "Reliable") {
    gradient.addColorStop(1, brightGreen);
    return;
  }
  if (link.status === "Possible") {
    gradient.addColorStop(degradationPoint, brightGreen);
    gradient.addColorStop(1, yellow);
    return;
  }

  gradient.addColorStop(Math.max(0.04, degradationPoint - 0.18), brightGreen);
  gradient.addColorStop(degradationPoint, yellow);
  gradient.addColorStop(Math.min(0.96, degradationPoint + 0.18), red);
  gradient.addColorStop(1, red);
}

function worldPositions(nodes: NetworkNode[], links: NetworkLinkResult[], topology: NetworkTopology, layoutSeed: number) {
  const positions = new Map<number, WorldPoint>();
  if (!nodes.length) return positions;

  if (topology === "Direct to hub") {
    positions.set(nodes[0].id, { x: 0, y: 0, z: altitudeToWorld(nodes[0].altitudeFeet) });
    for (let index = 1; index < nodes.length; index += 1) {
      const progress = index / Math.max(1, nodes.length - 1);
      const angle = index * 2.399963;
      const radius = Math.sqrt(progress);
      positions.set(nodes[index].id, {
        x: Math.cos(angle) * radius,
        y: Math.sin(angle) * radius,
        z: altitudeToWorld(nodes[index].altitudeFeet),
      });
    }
    return positions;
  }

  if (topology === "Random relay chain") {
    const gateway = nodes[0];
    positions.set(gateway.id, { x: 0, y: 0, z: altitudeToWorld(gateway.altitudeFeet) });
    const paths = randomBranchPaths(nodes, links);
    const longestBranch = Math.max(1, ...paths.map((path) => path.length - 1));
    const random = seededLayoutRandom(topologyLayoutHash(links) ^ layoutSeed);
    const layoutRotation = random() * Math.PI * 2;
    paths.forEach((path, branchIndex) => {
      const branchSpacing = Math.PI * 2 / Math.max(1, paths.length);
      const angleOffset = (random() - 0.5) * branchSpacing * 0.48;
      const branchBend = (random() - 0.5) * 0.42;
      const radiusScale = 0.86 + random() * 0.14;
      const baseAngle = layoutRotation + branchIndex * branchSpacing + angleOffset;
      path.slice(1).forEach((id, depthIndex) => {
        const node = nodes.find((candidate) => candidate.id === id)!;
        const depth = depthIndex + 1;
        const depthProgress = depth / longestBranch;
        const radius = (0.16 + depthProgress * 0.88) * radiusScale;
        const angle = baseAngle + branchBend * depthProgress + (random() - 0.5) * 0.1;
        positions.set(id, {
          x: Math.cos(angle) * radius,
          y: Math.sin(angle) * radius,
          z: altitudeToWorld(node.altitudeFeet),
        });
      });
    });
    return positions;
  }

  const orderedIds = chainOrder(nodes, links);
  const columns = Math.max(2, Math.ceil(Math.sqrt(orderedIds.length * 1.7)));
  const rows = Math.ceil(orderedIds.length / columns);
  orderedIds.forEach((id, index) => {
    const row = Math.floor(index / columns);
    const rawColumn = index % columns;
    const column = row % 2 === 0 ? rawColumn : columns - 1 - rawColumn;
    const node = nodes.find((candidate) => candidate.id === id)!;
    positions.set(id, {
      x: columns === 1 ? 0 : (column / Math.max(1, columns - 1)) * 2 - 1,
      y: rows === 1 ? 0 : (row / Math.max(1, rows - 1)) * 2 - 1,
      z: altitudeToWorld(node.altitudeFeet),
    });
  });
  return positions;
}

function perspectiveProject(point: WorldPoint, width: number, height: number, camera: Camera): ScreenPoint {
  const yawCos = Math.cos(camera.yaw);
  const yawSin = Math.sin(camera.yaw);
  const pitchSin = Math.sin(camera.pitch);
  const pitchCos = Math.cos(camera.pitch);
  const rotatedX = point.x * yawCos - point.y * yawSin;
  const rotatedY = point.x * yawSin + point.y * yawCos;
  const scale = Math.min(width / 2.45, height / 2.25) * camera.zoom;
  return {
    x: width / 2 + camera.panX + rotatedX * scale,
    y: height * 0.68 + camera.panY + (rotatedY * pitchSin - point.z * pitchCos) * scale,
    depth: rotatedY * pitchCos + point.z * pitchSin,
  };
}

function markerPath(context: CanvasRenderingContext2D, node: NetworkNode, point: ScreenPoint, radius: number) {
  context.beginPath();
  if (node.radioProfile === "series4000") {
    context.moveTo(point.x, point.y - radius - 1);
    context.lineTo(point.x + radius + 1, point.y);
    context.lineTo(point.x, point.y + radius + 1);
    context.lineTo(point.x - radius - 1, point.y);
    context.closePath();
  } else {
    const hexRadius = radius + 0.5;
    for (let index = 0; index < 6; index += 1) {
      const angle = Math.PI / 6 + index * Math.PI / 3;
      const x = point.x + Math.cos(angle) * hexRadius;
      const y = point.y + Math.sin(angle) * hexRadius;
      if (index === 0) context.moveTo(x, y);
      else context.lineTo(x, y);
    }
    context.closePath();
  }
}

function drawLabel(context: CanvasRenderingContext2D, text: string, x: number, y: number, emphasized: boolean) {
  context.font = `${emphasized ? 750 : 650} 10px ui-monospace, monospace`;
  const width = context.measureText(text).width + 10;
  context.fillStyle = "rgba(4, 12, 20, .88)";
  context.strokeStyle = emphasized ? "rgba(255,255,255,.58)" : "rgba(85,200,255,.28)";
  context.lineWidth = 1;
  context.beginPath();
  context.roundRect(x - width / 2, y - 11, width, 17, 4);
  context.fill();
  context.stroke();
  context.fillStyle = "rgba(229, 240, 247, .96)";
  context.textAlign = "center";
  context.fillText(text, x, y + 1);
}

export default function NetworkMap({
  nodes,
  links,
  topology,
  status,
  layoutSeed = 0,
  hasPendingChanges,
  onApplyChanges,
}: {
  nodes: NetworkNode[];
  links: NetworkLinkResult[];
  topology: NetworkTopology;
  status: "Reliable" | "Possible" | "Invalid";
  layoutSeed?: number;
  hasPendingChanges: boolean;
  onApplyChanges: () => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dragRef = useRef<{ x: number; y: number } | null>(null);
  const [camera, setCamera] = useState<Camera>(DEFAULT_CAMERA);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const draw = () => {
      const bounds = canvas.getBoundingClientRect();
      const width = Math.max(320, bounds.width);
      const height = Math.max(340, bounds.height);
      const pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
      const targetWidth = Math.round(width * pixelRatio);
      const targetHeight = Math.round(height * pixelRatio);
      if (canvas.width !== targetWidth) canvas.width = targetWidth;
      if (canvas.height !== targetHeight) canvas.height = targetHeight;
      const context = canvas.getContext("2d");
      if (!context) return;
      context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
      context.clearRect(0, 0, width, height);

      const sky = context.createLinearGradient(0, 0, 0, height);
      sky.addColorStop(0, "#071a2a");
      sky.addColorStop(0.62, "#081521");
      sky.addColorStop(1, "#040b12");
      context.fillStyle = sky;
      context.fillRect(0, 0, width, height);

      const planeCorners = [
        perspectiveProject({ x: -1.18, y: -1.18, z: 0 }, width, height, camera),
        perspectiveProject({ x: 1.18, y: -1.18, z: 0 }, width, height, camera),
        perspectiveProject({ x: 1.18, y: 1.18, z: 0 }, width, height, camera),
        perspectiveProject({ x: -1.18, y: 1.18, z: 0 }, width, height, camera),
      ];
      const terrain = context.createLinearGradient(0, planeCorners[0].y, 0, planeCorners[2].y);
      terrain.addColorStop(0, "rgba(17, 45, 55, .78)");
      terrain.addColorStop(1, "rgba(7, 24, 31, .96)");
      context.beginPath();
      context.moveTo(planeCorners[0].x, planeCorners[0].y);
      planeCorners.slice(1).forEach((point) => context.lineTo(point.x, point.y));
      context.closePath();
      context.fillStyle = terrain;
      context.fill();
      context.strokeStyle = "rgba(70, 133, 139, .38)";
      context.stroke();

      context.lineWidth = 1;
      for (let grid = -1; grid <= 1.001; grid += 0.2) {
        const horizontalStart = perspectiveProject({ x: -1.15, y: grid, z: 0 }, width, height, camera);
        const horizontalEnd = perspectiveProject({ x: 1.15, y: grid, z: 0 }, width, height, camera);
        const verticalStart = perspectiveProject({ x: grid, y: -1.15, z: 0 }, width, height, camera);
        const verticalEnd = perspectiveProject({ x: grid, y: 1.15, z: 0 }, width, height, camera);
        context.strokeStyle = Math.abs(grid) < 0.01 ? "rgba(85,200,255,.26)" : "rgba(91,133,145,.13)";
        context.beginPath();
        context.moveTo(horizontalStart.x, horizontalStart.y);
        context.lineTo(horizontalEnd.x, horizontalEnd.y);
        context.stroke();
        context.beginPath();
        context.moveTo(verticalStart.x, verticalStart.y);
        context.lineTo(verticalEnd.x, verticalEnd.y);
        context.stroke();
      }

      const worlds = worldPositions(nodes, links, topology, layoutSeed);
      const projected = new Map<number, { ground: ScreenPoint; top: ScreenPoint; world: WorldPoint }>();
      nodes.forEach((node) => {
        const world = worlds.get(node.id);
        if (!world) return;
        projected.set(node.id, {
          world,
          ground: perspectiveProject({ ...world, z: 0 }, width, height, camera),
          top: perspectiveProject(world, width, height, camera),
        });
      });

      for (const link of links) {
        const from = projected.get(link.from);
        const to = projected.get(link.to);
        if (!from || !to) continue;
        context.save();
        context.setLineDash([3, 5]);
        context.strokeStyle = "rgba(105, 137, 148, .16)";
        context.lineWidth = 1;
        context.beginPath();
        context.moveTo(from.ground.x, from.ground.y);
        context.lineTo(to.ground.x, to.ground.y);
        context.stroke();
        context.restore();

        const linkColor = link.status === "Reliable" ? "rgba(74, 222, 128, .92)" : link.status === "Possible" ? "rgba(251, 191, 36, .92)" : "rgba(251, 113, 133, .96)";
        const beam = context.createLinearGradient(from.top.x, from.top.y, to.top.x, to.top.y);
        addLinkHealthStops(beam, link);
        context.save();
        context.strokeStyle = beam;
        context.lineWidth = link.status === "Invalid" ? 2.3 : nodes.length > 75 ? 1 : 1.5;
        context.shadowColor = linkColor;
        context.shadowBlur = link.status === "Invalid" ? 8 : 4;
        context.beginPath();
        context.moveTo(from.top.x, from.top.y);
        context.lineTo(to.top.x, to.top.y);
        context.stroke();
        context.restore();
      }

      const invalidNodes = new Set<number>();
      links.forEach((link) => {
        if (link.status === "Invalid") {
          invalidNodes.add(link.from);
          invalidNodes.add(link.to);
        }
      });
      const radius = nodes.length > 80 ? 3.5 : nodes.length > 35 ? 4.5 : 6;
      const sortedNodes = [...nodes].sort((a, b) => (projected.get(a.id)?.ground.y ?? 0) - (projected.get(b.id)?.ground.y ?? 0));
      sortedNodes.forEach((node) => {
        const points = projected.get(node.id);
        if (!points) return;
        context.save();
        context.fillStyle = "rgba(0,0,0,.3)";
        context.beginPath();
        context.ellipse(points.ground.x, points.ground.y + 2, radius * 1.4, radius * .5, 0, 0, Math.PI * 2);
        context.fill();

        const stemColor = node.radioProfile === "sl5200" ? "rgba(45,212,191,.58)" : "rgba(85,200,255,.58)";
        context.strokeStyle = invalidNodes.has(node.id) ? "rgba(251,113,133,.8)" : stemColor;
        context.lineWidth = node.id === 1 ? 2.2 : 1.2;
        context.beginPath();
        context.moveTo(points.ground.x, points.ground.y);
        context.lineTo(points.top.x, points.top.y);
        context.stroke();
        if (node.altitudeFeet > 100 && nodes.length <= 75) {
          for (let level = .28; level < 1; level += .28) {
            const x = points.ground.x + (points.top.x - points.ground.x) * level;
            const y = points.ground.y + (points.top.y - points.ground.y) * level;
            context.strokeStyle = "rgba(151,183,197,.24)";
            context.beginPath();
            context.moveTo(x - 3, y);
            context.lineTo(x + 3, y);
            context.stroke();
          }
        }

        markerPath(context, node, points.top, node.id === 1 ? radius + 2 : radius);
        const nodeColor = node.radioProfile === "sl5200" ? "#2dd4bf" : "#55c8ff";
        const fill = context.createRadialGradient(points.top.x - radius * .35, points.top.y - radius * .45, 1, points.top.x, points.top.y, radius * 1.5);
        fill.addColorStop(0, "rgba(255,255,255,.95)");
        fill.addColorStop(.22, nodeColor);
        fill.addColorStop(1, node.radioProfile === "sl5200" ? "#0b7f78" : "#126c9b");
        context.fillStyle = fill;
        context.fill();
        context.strokeStyle = invalidNodes.has(node.id) ? "#fb7185" : node.id === 1 ? "#ffffff" : "rgba(219,235,244,.7)";
        context.lineWidth = invalidNodes.has(node.id) || node.id === 1 ? 2 : 1;
        context.stroke();
        context.restore();

        if (nodes.length <= 30 || node.id === 1) {
          drawLabel(context, node.id === 1 ? "GATEWAY" : `${node.id} · ${node.altitudeFeet}ft`, points.top.x, points.top.y - radius - 9, node.id === 1);
        }
      });

      const axisX = width - 68;
      const axisY = height - 38;
      context.lineWidth = 2;
      const axes = [
        { dx: Math.cos(camera.yaw) * 24, dy: Math.sin(camera.yaw) * Math.sin(camera.pitch) * 24, color: "#55c8ff", label: "X" },
        { dx: -Math.sin(camera.yaw) * 24, dy: Math.cos(camera.yaw) * Math.sin(camera.pitch) * 24, color: "#2dd4bf", label: "Y" },
        { dx: 0, dy: -Math.cos(camera.pitch) * 30, color: "#fbbf24", label: "ALT" },
      ];
      axes.forEach((axis) => {
        context.strokeStyle = axis.color;
        context.beginPath();
        context.moveTo(axisX, axisY);
        context.lineTo(axisX + axis.dx, axisY + axis.dy);
        context.stroke();
        context.fillStyle = "rgba(226,238,246,.9)";
        context.font = "650 9px ui-monospace, monospace";
        context.textAlign = "center";
        context.fillText(axis.label, axisX + axis.dx * 1.25, axisY + axis.dy * 1.25 + 3);
      });
    };

    let animationFrame = 0;
    const requestDraw = () => {
      cancelAnimationFrame(animationFrame);
      animationFrame = requestAnimationFrame(draw);
    };
    draw();
    const resizeObserver = new ResizeObserver(requestDraw);
    resizeObserver.observe(canvas);
    return () => {
      resizeObserver.disconnect();
      cancelAnimationFrame(animationFrame);
    };
  }, [camera, layoutSeed, links, nodes, status, topology]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const handleWheel = (event: WheelEvent) => {
      event.preventDefault();
      event.stopPropagation();
      const bounds = canvas.getBoundingClientRect();
      const pointerX = event.clientX - bounds.left;
      const pointerY = event.clientY - bounds.top;
      setCamera((current) => {
        const nextZoom = clamp(current.zoom * Math.exp(-event.deltaY * .0015), MIN_CAMERA_ZOOM, MAX_CAMERA_ZOOM);
        const zoomRatio = nextZoom / current.zoom;
        const centerX = bounds.width / 2;
        const centerY = bounds.height * .68;
        return {
          ...current,
          zoom: nextZoom,
          panX: pointerX - centerX - (pointerX - centerX - current.panX) * zoomRatio,
          panY: pointerY - centerY - (pointerY - centerY - current.panY) * zoomRatio,
        };
      });
    };

    canvas.addEventListener("wheel", handleWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", handleWheel);
  }, []);

  const onPointerDown = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    dragRef.current = { x: event.clientX, y: event.clientY };
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    const previous = dragRef.current;
    if (!previous) return;
    const deltaX = event.clientX - previous.x;
    const deltaY = event.clientY - previous.y;
    dragRef.current = { x: event.clientX, y: event.clientY };
    setCamera((current) => ({
      ...current,
      yaw: current.yaw + deltaX * .009,
      pitch: clamp(current.pitch + deltaY * .006, .26, 1.18),
    }));
  };

  const stopDragging = (event: ReactPointerEvent<HTMLCanvasElement>) => {
    dragRef.current = null;
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId);
  };

  return (
    <section className={`network-map-card network-map-card--3d${status === "Invalid" ? " network-map-card--invalid" : ""}${hasPendingChanges ? " network-map-card--pending" : ""}`}>
      <div className="network-map-heading">
        <div><span className="section-kicker">Interactive 3D topology</span><h2>3D multi-node network map</h2><p>Review the live calculations, then apply pending changes to redraw this map.</p></div>
        <div className="network-map-actions">
          <div className="network-map-command-bar">
            <div className="network-map-readouts"><span>{topologyLabel(topology)}</span><strong>{nodes.length} nodes</strong></div>
            <button className={`network-map-apply-button${hasPendingChanges ? " network-map-apply-button--pending" : ""}`} disabled={!hasPendingChanges} onClick={onApplyChanges} type="button">{hasPendingChanges ? "Apply changes" : "Map up to date"}</button>
          </div>
          <div className="network-view-controls" aria-label="3D map view controls">
            <button aria-label="Rotate view left" onClick={() => setCamera((current) => ({ ...current, yaw: current.yaw - .25 }))} type="button">Rotate −</button>
            <button onClick={() => setCamera(DEFAULT_CAMERA)} type="button">Reset view</button>
            <button aria-label="Rotate view right" onClick={() => setCamera((current) => ({ ...current, yaw: current.yaw + .25 }))} type="button">Rotate +</button>
            <button aria-label="Lower viewing angle" onClick={() => setCamera((current) => ({ ...current, pitch: clamp(current.pitch - .1, .26, 1.18) }))} type="button">Tilt −</button>
            <button aria-label="Raise viewing angle" onClick={() => setCamera((current) => ({ ...current, pitch: clamp(current.pitch + .1, .26, 1.18) }))} type="button">Tilt +</button>
            <button aria-label="Zoom out" onClick={() => setCamera((current) => ({ ...current, zoom: clamp(current.zoom - .25, MIN_CAMERA_ZOOM, MAX_CAMERA_ZOOM) }))} type="button">−</button>
            <output aria-label="Map zoom level" className="network-zoom-readout">{camera.zoom.toFixed(1)}×</output>
            <button aria-label="Zoom in" onClick={() => setCamera((current) => ({ ...current, zoom: clamp(current.zoom + .25, MIN_CAMERA_ZOOM, MAX_CAMERA_ZOOM) }))} type="button">+</button>
          </div>
        </div>
      </div>
      <canvas
        aria-label={`Interactive 3D ${topology} network map with ${nodes.length} nodes and ${links.length} modeled links. Node altitude is shown vertically. Network status: ${status}.`}
        className="network-map-canvas network-map-canvas--3d"
        onPointerCancel={stopDragging}
        onPointerDown={onPointerDown}
        onPointerLeave={stopDragging}
        onPointerMove={onPointerMove}
        onPointerUp={stopDragging}
        ref={canvasRef}
        role="img"
      >
        Interactive 3D schematic of the multi-node radio network.
      </canvas>
      <div className="network-map-instructions">Drag to orbit · Scroll over a connection to zoom toward it (up to 5×) · Altitude stems use a logarithmic scale</div>
      <div className="network-map-legend" aria-label="Network map legend">
        <span><i className="network-swatch network-swatch--4000" />4000 Series · diamond</span>
        <span><i className="network-swatch network-swatch--5200" />SL5200 estimated · hexagon</span>
        <span><i className="network-line network-line--reliable" />Strong throughout</span>
        <span><i className="network-line network-line--possible" />Weakens near node</span>
        <span><i className="network-line network-line--invalid" />Degrades to failed</span>
        <span><i className="network-altitude-mark" />Altitude</span>
      </div>
    </section>
  );
}
