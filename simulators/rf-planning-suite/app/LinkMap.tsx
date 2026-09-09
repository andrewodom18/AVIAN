"use client";

import { useEffect, useRef } from "react";
import { MAX_TX_HEIGHT_FEET } from "./model";

type LinkMapProps = {
  blocked: boolean;
  distanceKm: number;
  environment: string;
  fresnelRadiusMeters: number;
  horizonKm: number;
  rxHeightFeet: number;
  status: "Reliable" | "Possible" | "No viable mode";
  txHeightFeet: number;
};

const TERRAIN_ROUGHNESS: Record<string, number> = {
  "Free Space": 0.05,
  "Air to Air": 0.05,
  "Air to Ground": 0.12,
  Maritime: 0.04,
  Rural: 0.28,
  "Urban - Raised Antennas": 0.36,
  "Urban - Low Antennas": 0.52,
  "Ground Robotics": 0.44,
};

type AntennaKind = "blade" | "dish" | "omni" | "panel" | "whip" | "yagi";

const ENVIRONMENT_SCENES: Record<
  string,
  {
    antennaTx: AntennaKind;
    antennaRx: AntennaKind;
    backdropTop: string;
    backdropBottom: string;
    txLabel: string;
    rxLabel: string;
  }
> = {
  "Free Space": {
    antennaTx: "dish",
    antennaRx: "dish",
    backdropTop: "#050b18",
    backdropBottom: "#101b2b",
    txLabel: "Tracking dish",
    rxLabel: "Tracking dish",
  },
  "Air to Air": {
    antennaTx: "blade",
    antennaRx: "blade",
    backdropTop: "#071b30",
    backdropBottom: "#28536b",
    txLabel: "Aircraft blade antenna",
    rxLabel: "Aircraft blade antenna",
  },
  "Air to Ground": {
    antennaTx: "blade",
    antennaRx: "dish",
    backdropTop: "#0a2035",
    backdropBottom: "#183b51",
    txLabel: "Ventral blade antenna",
    rxLabel: "Tracking dish",
  },
  Maritime: {
    antennaTx: "omni",
    antennaRx: "omni",
    backdropTop: "#081b2d",
    backdropBottom: "#16405a",
    txLabel: "Marine collinear",
    rxLabel: "Marine collinear",
  },
  Rural: {
    antennaTx: "yagi",
    antennaRx: "panel",
    backdropTop: "#071a28",
    backdropBottom: "#173a3a",
    txLabel: "Directional Yagi",
    rxLabel: "Sector panel",
  },
  "Urban - Raised Antennas": {
    antennaTx: "panel",
    antennaRx: "panel",
    backdropTop: "#081625",
    backdropBottom: "#202f43",
    txLabel: "Rooftop panel",
    rxLabel: "Rooftop panel",
  },
  "Urban - Low Antennas": {
    antennaTx: "omni",
    antennaRx: "panel",
    backdropTop: "#0a1420",
    backdropBottom: "#29313b",
    txLabel: "Street-level omni",
    rxLabel: "Low sector panel",
  },
  "Ground Robotics": {
    antennaTx: "whip",
    antennaRx: "whip",
    backdropTop: "#111a22",
    backdropBottom: "#43352a",
    txLabel: "Vehicle whip",
    rxLabel: "Vehicle whip",
  },
};

function roundedRect(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
) {
  const r = Math.min(radius, width / 2, height / 2);
  context.beginPath();
  context.moveTo(x + r, y);
  context.lineTo(x + width - r, y);
  context.quadraticCurveTo(x + width, y, x + width, y + r);
  context.lineTo(x + width, y + height - r);
  context.quadraticCurveTo(x + width, y + height, x + width - r, y + height);
  context.lineTo(x + r, y + height);
  context.quadraticCurveTo(x, y + height, x, y + height - r);
  context.lineTo(x, y + r);
  context.quadraticCurveTo(x, y, x + r, y);
  context.closePath();
}

function drawCloud(context: CanvasRenderingContext2D, x: number, y: number, scale: number) {
  context.save();
  context.globalAlpha = 0.16;
  context.fillStyle = "#c7e9f7";
  context.beginPath();
  context.arc(x, y, 10 * scale, 0, Math.PI * 2);
  context.arc(x + 12 * scale, y - 5 * scale, 13 * scale, 0, Math.PI * 2);
  context.arc(x + 27 * scale, y, 10 * scale, 0, Math.PI * 2);
  context.fill();
  context.restore();
}

function drawMountains(context: CanvasRenderingContext2D, width: number, groundBase: number) {
  const farGradient = context.createLinearGradient(0, groundBase - 110, 0, groundBase);
  farGradient.addColorStop(0, "rgba(81, 113, 118, .12)");
  farGradient.addColorStop(1, "rgba(39, 82, 78, .34)");
  context.fillStyle = farGradient;
  context.beginPath();
  context.moveTo(0, groundBase);
  context.lineTo(width * 0.09, groundBase - 48);
  context.lineTo(width * 0.2, groundBase - 22);
  context.lineTo(width * 0.35, groundBase - 91);
  context.lineTo(width * 0.48, groundBase - 31);
  context.lineTo(width * 0.63, groundBase - 73);
  context.lineTo(width * 0.77, groundBase - 26);
  context.lineTo(width * 0.9, groundBase - 63);
  context.lineTo(width, groundBase - 35);
  context.lineTo(width, groundBase);
  context.closePath();
  context.fill();

  const nearGradient = context.createLinearGradient(0, groundBase - 62, 0, groundBase + 5);
  nearGradient.addColorStop(0, "rgba(26, 73, 69, .26)");
  nearGradient.addColorStop(1, "rgba(18, 55, 52, .72)");
  context.fillStyle = nearGradient;
  context.beginPath();
  context.moveTo(0, groundBase);
  context.quadraticCurveTo(width * .12, groundBase - 46, width * .27, groundBase - 19);
  context.quadraticCurveTo(width * .43, groundBase - 57, width * .58, groundBase - 23);
  context.quadraticCurveTo(width * .75, groundBase - 51, width, groundBase - 12);
  context.lineTo(width, groundBase);
  context.closePath();
  context.fill();
}

function drawAircraft(context: CanvasRenderingContext2D, x: number, y: number, color: string, direction: -1 | 1 = 1) {
  context.save();
  context.translate(x, y);
  context.scale(direction, 1);
  context.fillStyle = color;
  context.globalAlpha = .96;

  // Generic utility turboprop profile: steep windscreen, long cabin, and tapered tail cone.
  context.beginPath();
  context.moveTo(-51, -1);
  context.lineTo(-44, -6);
  context.lineTo(-31, -8);
  context.lineTo(-22, -14);
  context.lineTo(-13, -15);
  context.lineTo(23, -14);
  context.quadraticCurveTo(34, -11, 42, -6);
  context.lineTo(47, -2);
  context.lineTo(44, 3);
  context.lineTo(31, 7);
  context.lineTo(18, 10);
  context.lineTo(-28, 11);
  context.quadraticCurveTo(-43, 9, -51, 4);
  context.closePath();
  context.fill();

  // Foreshortened high wing and its root fairing.
  context.beginPath();
  context.moveTo(-12, -16);
  context.lineTo(19, -15);
  context.lineTo(24, -11);
  context.lineTo(-15, -9);
  context.closePath();
  context.fill();
  context.beginPath();
  context.moveTo(-7, -10);
  context.lineTo(12, -9);
  context.lineTo(15, 1);
  context.lineTo(4, 5);
  context.lineTo(-10, -2);
  context.closePath();
  context.fill();

  // Swept vertical fin and broad horizontal stabilizer form the characteristic T-tail.
  context.beginPath();
  context.moveTo(28, -10);
  context.lineTo(41, -36);
  context.lineTo(49, -35);
  context.lineTo(47, -3);
  context.closePath();
  context.fill();
  context.beginPath();
  context.moveTo(27, -37);
  context.lineTo(55, -36);
  context.lineTo(59, -32);
  context.lineTo(26, -31);
  context.closePath();
  context.fill();

  // Pointed spinner and edge-on five-blade propeller disc in a true side profile.
  context.beginPath();
  context.moveTo(-49, -4);
  context.lineTo(-59, 0);
  context.lineTo(-49, 5);
  context.closePath();
  context.fill();
  context.save();
  context.translate(-57, 0);
  context.beginPath();
  context.moveTo(-1, -2);
  context.quadraticCurveTo(-4, -12, -1, -22);
  context.lineTo(2, -22);
  context.quadraticCurveTo(1, -10, 2, -2);
  context.closePath();
  context.fill();
  context.beginPath();
  context.moveTo(1, 2);
  context.quadraticCurveTo(4, 12, 1, 22);
  context.lineTo(-2, 22);
  context.quadraticCurveTo(-1, 10, -2, 2);
  context.closePath();
  context.fill();
  context.beginPath();
  context.arc(0, 0, 3, 0, Math.PI * 2);
  context.fill();
  context.restore();

  // Compact EO/IR mission fairing beneath the cabin.
  context.beginPath();
  context.ellipse(18, 12, 7, 4, 0, 0, Math.PI * 2);
  context.fill();
  context.restore();
}

function drawVentralBladeAntenna(
  context: CanvasRenderingContext2D,
  x: number,
  tipY: number,
  color: string,
) {
  context.save();
  context.fillStyle = color;
  context.beginPath();
  context.moveTo(x - 3, tipY - 9);
  context.lineTo(x + 3, tipY - 9);
  context.lineTo(x + 1, tipY);
  context.lineTo(x, tipY + 2);
  context.lineTo(x - 1, tipY);
  context.closePath();
  context.fill();
  context.restore();
}

function drawShip(context: CanvasRenderingContext2D, x: number, waterY: number, color: string) {
  context.save();
  context.fillStyle = "rgba(177, 205, 217, .82)";
  context.strokeStyle = color;
  context.lineWidth = 1.5;
  context.beginPath();
  context.moveTo(x - 26, waterY - 7);
  context.lineTo(x + 25, waterY - 7);
  context.lineTo(x + 17, waterY + 4);
  context.lineTo(x - 17, waterY + 4);
  context.closePath();
  context.fill();
  context.stroke();
  context.fillStyle = "rgba(28, 59, 73, .95)";
  context.fillRect(x - 10, waterY - 18, 20, 11);
  context.fillStyle = "rgba(85, 200, 255, .45)";
  context.fillRect(x - 6, waterY - 15, 4, 3);
  context.fillRect(x + 2, waterY - 15, 4, 3);
  context.restore();
}

function drawRover(context: CanvasRenderingContext2D, x: number, ground: number, color: string) {
  context.save();
  context.fillStyle = "rgba(137, 158, 164, .92)";
  context.strokeStyle = color;
  context.lineWidth = 1.5;
  roundedRect(context, x - 20, ground - 18, 40, 14, 4);
  context.fill();
  context.stroke();
  context.fillStyle = "#0a1720";
  for (const offset of [-14, 0, 14]) {
    context.beginPath();
    context.arc(x + offset, ground - 2, 5, 0, Math.PI * 2);
    context.fill();
    context.stroke();
  }
  context.restore();
}

function drawAntenna(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  direction: -1 | 1,
  kind: AntennaKind,
  color: string,
) {
  context.save();
  context.translate(x, y);
  context.strokeStyle = color;
  context.fillStyle = color;
  context.lineWidth = 1.8;

  if (kind === "dish") {
    context.save();
    context.scale(direction, 1);
    context.beginPath();
    context.ellipse(0, 0, 10, 5, -0.42, Math.PI * 0.48, Math.PI * 1.5);
    context.stroke();
    context.beginPath();
    context.moveTo(1, 1);
    context.lineTo(10, -5);
    context.stroke();
    context.beginPath();
    context.arc(11, -6, 2, 0, Math.PI * 2);
    context.fill();
    context.restore();
  } else if (kind === "panel") {
    roundedRect(context, direction > 0 ? -2 : -6, -9, 8, 18, 2);
    context.fillStyle = "rgba(174, 230, 236, .92)";
    context.fill();
    context.stroke();
    context.beginPath();
    context.moveTo(direction > 0 ? -2 : 2, 0);
    context.lineTo(direction > 0 ? -8 : 8, 4);
    context.stroke();
  } else if (kind === "yagi") {
    context.beginPath();
    context.moveTo(0, 0);
    context.lineTo(direction * 18, 0);
    for (let index = 3; index <= 16; index += 4) {
      context.moveTo(direction * index, -6);
      context.lineTo(direction * index, 6);
    }
    context.stroke();
  } else if (kind === "blade") {
    context.beginPath();
    context.moveTo(-3, 1);
    context.lineTo(-1, -9);
    context.lineTo(3, -8);
    context.lineTo(4, 1);
    context.closePath();
    context.fill();
  } else {
    const height = kind === "whip" ? 21 : 16;
    context.beginPath();
    context.moveTo(0, 2);
    context.lineTo(direction * 1.5, -height);
    context.stroke();
    context.beginPath();
    context.arc(0, 2, 3, 0, Math.PI * 2);
    context.fill();
  }
  context.restore();
}

function drawLatticeTower(
  context: CanvasRenderingContext2D,
  x: number,
  ground: number,
  top: number,
  color: string,
) {
  const towerHeight = ground - top;
  const baseHalf = Math.max(9, Math.min(17, towerHeight * 0.18));
  context.save();
  context.strokeStyle = color;
  context.lineWidth = 1.6;
  context.beginPath();
  context.moveTo(x - baseHalf, ground);
  context.lineTo(x - 2, top);
  context.lineTo(x + 2, top);
  context.lineTo(x + baseHalf, ground);
  context.moveTo(x - baseHalf, ground);
  context.lineTo(x + baseHalf, ground);
  const sections = 5;
  for (let section = 1; section < sections; section += 1) {
    const progress = section / sections;
    const y = ground - towerHeight * progress;
    const half = baseHalf * (1 - progress) + 2 * progress;
    context.moveTo(x - half, y);
    context.lineTo(x + half, y);
  }
  for (let section = 0; section < sections; section += 1) {
    const firstProgress = section / sections;
    const secondProgress = (section + 1) / sections;
    const firstY = ground - towerHeight * firstProgress;
    const secondY = ground - towerHeight * secondProgress;
    const firstHalf = baseHalf * (1 - firstProgress) + 2 * firstProgress;
    const secondHalf = baseHalf * (1 - secondProgress) + 2 * secondProgress;
    context.moveTo(x - firstHalf, firstY);
    context.lineTo(x + secondHalf, secondY);
    context.moveTo(x + firstHalf, firstY);
    context.lineTo(x - secondHalf, secondY);
  }
  context.stroke();
  context.fillStyle = "rgba(85, 200, 255, .1)";
  context.fillRect(x - 10, top + 7, 20, 3);
  context.restore();
}

export default function LinkMap({
  blocked,
  distanceKm,
  environment,
  fresnelRadiusMeters,
  horizonKm,
  rxHeightFeet,
  status,
  txHeightFeet,
}: LinkMapProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const scene = ENVIRONMENT_SCENES[environment] ?? ENVIRONMENT_SCENES.Rural;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const draw = () => {
      const bounds = canvas.getBoundingClientRect();
      const width = Math.max(320, bounds.width);
      const height = Math.max(220, bounds.height);
      const pixelRatio = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.round(width * pixelRatio);
      canvas.height = Math.round(height * pixelRatio);
      const context = canvas.getContext("2d");
      if (!context) return;
      context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
      context.clearRect(0, 0, width, height);

      const statusColor = status === "Reliable" ? "#2dd4bf" : status === "Possible" ? "#fbbf24" : "#fb7185";
      const edgeInset = Math.max(58, width * 0.07);
      const airToAir = environment === "Air to Air";
      const left = environment === "Air to Ground" || airToAir ? Math.max(78, width * 0.09) : edgeInset;
      const right = width - (airToAir ? Math.max(78, width * 0.09) : edgeInset);
      const top = 26;
      const groundBase = height * 0.76;
      const roughness = TERRAIN_ROUGHNESS[environment] ?? 0.25;
      const scene = ENVIRONMENT_SCENES[environment] ?? ENVIRONMENT_SCENES.Rural;

      const backdrop = context.createLinearGradient(0, 0, 0, height);
      backdrop.addColorStop(0, scene.backdropTop);
      backdrop.addColorStop(0.7, scene.backdropBottom);
      backdrop.addColorStop(1, "#0a1720");
      context.fillStyle = backdrop;
      context.fillRect(0, 0, width, height);

      if (environment === "Free Space") {
        context.fillStyle = "rgba(184, 224, 242, .62)";
        for (let index = 0; index < 34; index += 1) {
          const starX = ((index * 83) % Math.max(1, width - 30)) + 15;
          const starY = ((index * 47) % Math.max(1, groundBase - 35)) + 14;
          const radius = index % 7 === 0 ? 1.4 : 0.7;
          context.beginPath();
          context.arc(starX, starY, radius, 0, Math.PI * 2);
          context.fill();
        }
        const planetGlow = context.createRadialGradient(width * 0.78, groundBase + 55, 4, width * 0.78, groundBase + 55, 100);
        planetGlow.addColorStop(0, "rgba(85, 200, 255, .24)");
        planetGlow.addColorStop(1, "rgba(85, 200, 255, 0)");
        context.fillStyle = planetGlow;
        context.fillRect(width * 0.58, groundBase - 45, width * 0.4, 160);
      } else if (environment === "Air to Ground" || environment === "Air to Air") {
        drawCloud(context, width * 0.22, top + 36, 0.8);
        drawCloud(context, width * 0.6, top + 58, 1.15);
        drawCloud(context, width * 0.82, top + 28, 0.65);
        const sunGlow = context.createRadialGradient(width * 0.82, top + 20, 2, width * 0.82, top + 20, 34);
        sunGlow.addColorStop(0, "rgba(251, 191, 36, .4)");
        sunGlow.addColorStop(1, "rgba(251, 191, 36, 0)");
        context.fillStyle = sunGlow;
        context.fillRect(width * 0.74, top - 8, width * 0.16, 70);
      } else if (environment === "Rural") {
        const horizonGlow = context.createLinearGradient(0, groundBase - 115, 0, groundBase + 5);
        horizonGlow.addColorStop(0, "rgba(238, 180, 104, 0)");
        horizonGlow.addColorStop(.72, "rgba(238, 180, 104, .10)");
        horizonGlow.addColorStop(1, "rgba(164, 142, 83, .04)");
        context.fillStyle = horizonGlow;
        context.fillRect(0, groundBase - 115, width, 120);
        drawCloud(context, width * .16, top + 41, .55);
        drawCloud(context, width * .72, top + 26, .45);
        drawMountains(context, width, groundBase);
      } else if (environment === "Ground Robotics") {
        const haze = context.createLinearGradient(0, groundBase - 90, 0, groundBase + 10);
        haze.addColorStop(0, "rgba(201, 139, 85, 0)");
        haze.addColorStop(1, "rgba(201, 139, 85, .13)");
        context.fillStyle = haze;
        context.fillRect(0, groundBase - 90, width, 110);
      }

      context.strokeStyle = "rgba(78, 115, 143, 0.16)";
      context.lineWidth = 1;
      for (let index = 0; index <= 10; index += 1) {
        const x = left + ((right - left) * index) / 10;
        context.beginPath();
        context.moveTo(x, top);
        context.lineTo(x, groundBase + 34);
        context.stroke();
      }
      for (let index = 0; index <= 4; index += 1) {
        const y = top + ((groundBase - top) * index) / 4;
        context.beginPath();
        context.moveTo(left, y);
        context.lineTo(right, y);
        context.stroke();
      }

      const terrainY = (progress: number) => {
        if (environment === "Maritime") return groundBase + Math.sin(progress * Math.PI * 6) * 1.5;
        const broad = Math.sin(progress * Math.PI * 2.4 + 0.5) * 16;
        const local = Math.sin(progress * Math.PI * 8.2 + 1.1) * 7;
        const detail = Math.sin(progress * Math.PI * 17.5) * 3;
        return groundBase - (broad + local + detail) * roughness;
      };

      context.beginPath();
      context.moveTo(left, height);
      for (let index = 0; index <= 80; index += 1) {
        const progress = index / 80;
        context.lineTo(left + (right - left) * progress, terrainY(progress));
      }
      context.lineTo(right, height);
      context.closePath();
      const terrainFill = context.createLinearGradient(0, groundBase - 35, 0, height);
      if (environment === "Maritime") {
        terrainFill.addColorStop(0, "rgba(35, 119, 150, .84)");
        terrainFill.addColorStop(1, "rgba(7, 37, 58, .98)");
      } else if (environment === "Ground Robotics") {
        terrainFill.addColorStop(0, "rgba(103, 77, 56, .96)");
        terrainFill.addColorStop(1, "rgba(35, 29, 26, .98)");
      } else if (environment === "Rural") {
        terrainFill.addColorStop(0, "rgba(45, 91, 69, .96)");
        terrainFill.addColorStop(.48, "rgba(29, 67, 53, .98)");
        terrainFill.addColorStop(1, "rgba(10, 32, 30, .99)");
      } else {
        terrainFill.addColorStop(0, "rgba(37, 75, 85, 0.92)");
        terrainFill.addColorStop(1, "rgba(10, 27, 34, 0.98)");
      }
      context.fillStyle = terrainFill;
      context.fill();
      context.strokeStyle = environment === "Maritime" ? "rgba(85, 200, 255, .68)" : "rgba(74, 154, 149, 0.58)";
      context.lineWidth = 1.5;
      context.stroke();

      if (environment.includes("Urban")) {
        const buildingCount = width < 600 ? 9 : 17;
        for (let index = 1; index < buildingCount; index += 1) {
          const progress = index / buildingCount;
          const x = left + (right - left) * progress;
          const buildingWidth = width < 600 ? 15 : 22;
          const buildingHeight = 25 + ((index * 23) % 62) * (environment.includes("Raised") ? 1 : .72);
          const buildingY = terrainY(progress) - buildingHeight;
          const facade = context.createLinearGradient(x, buildingY, x + buildingWidth, buildingY);
          facade.addColorStop(0, "rgba(28, 48, 64, .96)");
          facade.addColorStop(1, "rgba(49, 70, 86, .92)");
          context.fillStyle = facade;
          context.fillRect(x - buildingWidth / 2, buildingY, buildingWidth, buildingHeight);
          context.strokeStyle = "rgba(99, 132, 153, .26)";
          context.strokeRect(x - buildingWidth / 2, buildingY, buildingWidth, buildingHeight);
          context.fillStyle = "rgba(251, 191, 36, .34)";
          for (let floor = 8; floor < buildingHeight - 5; floor += 10) {
            context.fillRect(x - buildingWidth / 2 + 4, buildingY + floor, 3, 3);
            context.fillRect(x + buildingWidth / 2 - 7, buildingY + floor, 3, 3);
          }
          if (index % 4 === 0) {
            context.strokeStyle = "rgba(85, 200, 255, .5)";
            context.beginPath();
            context.moveTo(x, buildingY);
            context.lineTo(x, buildingY - 10);
            context.stroke();
          }
        }
      } else if (environment === "Rural") {
        context.strokeStyle = "rgba(103, 148, 99, .2)";
        context.lineWidth = 1;
        for (let row = 1; row <= 5; row += 1) {
          context.beginPath();
          for (let index = 0; index <= 50; index += 1) {
            const progress = index / 50;
            const x = left + (right - left) * progress;
            const y = terrainY(progress) + 8 + row * row * 2.3;
            if (index === 0) context.moveTo(x, y);
            else context.lineTo(x, y);
          }
          context.stroke();
        }

        context.strokeStyle = "rgba(66, 132, 95, .72)";
        context.fillStyle = "rgba(36, 103, 75, .88)";
        for (let index = 1; index < 16; index += 1) {
          const progress = index / 16;
          const x = left + (right - left) * progress;
          const y = terrainY(progress);
          const treeHeight = 10 + ((index * 7) % 13);
          const crown = 4 + (index % 3);
          context.beginPath();
          context.moveTo(x, y);
          context.lineTo(x, y - treeHeight);
          context.stroke();
          context.beginPath();
          context.arc(x - crown * .45, y - treeHeight - crown * .55, crown, 0, Math.PI * 2);
          context.arc(x + crown * .55, y - treeHeight - crown * .4, crown * .9, 0, Math.PI * 2);
          context.arc(x, y - treeHeight - crown * 1.15, crown * .9, 0, Math.PI * 2);
          context.fill();
        }

        const barnX = left + (right - left) * .61;
        const barnGround = terrainY(.61);
        context.fillStyle = "rgba(104, 71, 55, .94)";
        context.fillRect(barnX - 15, barnGround - 16, 30, 16);
        context.fillStyle = "rgba(68, 48, 43, .98)";
        context.beginPath();
        context.moveTo(barnX - 18, barnGround - 16);
        context.lineTo(barnX, barnGround - 28);
        context.lineTo(barnX + 18, barnGround - 16);
        context.closePath();
        context.fill();
        context.fillStyle = "rgba(236, 187, 111, .26)";
        context.fillRect(barnX - 4, barnGround - 12, 8, 12);

        context.strokeStyle = "rgba(169, 145, 99, .48)";
        context.lineWidth = 1;
        for (let index = 2; index < 15; index += 2) {
          const progress = index / 16;
          const x = left + (right - left) * progress;
          const y = terrainY(progress);
          context.beginPath();
          context.moveTo(x, y + 1);
          context.lineTo(x, y - 9);
          context.stroke();
        }
      } else if (environment === "Maritime") {
        context.strokeStyle = "rgba(136, 220, 244, .3)";
        for (let wave = 0; wave < 4; wave += 1) {
          context.beginPath();
          for (let index = 0; index <= 60; index += 1) {
            const progress = index / 60;
            const x = left + (right - left) * progress;
            const y = groundBase + 9 + wave * 9 + Math.sin(progress * Math.PI * 8 + wave) * 2;
            if (index === 0) context.moveTo(x, y);
            else context.lineTo(x, y);
          }
          context.stroke();
        }
      } else if (environment === "Ground Robotics") {
        context.fillStyle = "rgba(154, 116, 82, .72)";
        for (let index = 1; index < 9; index += 1) {
          const progress = index / 9;
          const x = left + (right - left) * progress;
          const y = terrainY(progress);
          context.beginPath();
          context.ellipse(x, y - 2, 5 + (index % 3) * 2, 3 + (index % 2), -.2, 0, Math.PI * 2);
          context.fill();
        }
      }

      const txGround = terrainY(0);
      const rxGround = terrainY(1);
      const visualHeight = (feet: number) => 30 + Math.min(52, Math.log10(Math.max(1, feet)) * 16);
      const airborneAntennaY = (feet: number) => {
        const boundedAltitude = Math.max(1, Math.min(MAX_TX_HEIGHT_FEET, feet));
        const logarithmicAltitude = Math.log1p(boundedAltitude) / Math.log1p(MAX_TX_HEIGHT_FEET);
        const altitudeProgress = Math.pow(logarithmicAltitude, .72);
        const highestAntennaY = top + 42;
        const lowestAntennaY = groundBase - 30;
        return lowestAntennaY - (lowestAntennaY - highestAntennaY) * altitudeProgress;
      };
      let txTop = txGround - visualHeight(txHeightFeet);
      let rxTop = rxGround - visualHeight(rxHeightFeet);
      if (environment === "Air to Ground") {
        txTop = airborneAntennaY(txHeightFeet);
        rxTop = rxGround - Math.max(42, visualHeight(rxHeightFeet));
      } else if (environment === "Air to Air") {
        txTop = airborneAntennaY(txHeightFeet);
        rxTop = airborneAntennaY(rxHeightFeet);
      } else if (environment === "Maritime") {
        txTop = txGround - 42;
        rxTop = rxGround - 42;
      } else if (environment === "Ground Robotics") {
        txTop = txGround - 39;
        rxTop = rxGround - 39;
      } else if (environment === "Urban - Low Antennas") {
        txTop = txGround - 34;
        rxTop = rxGround - 38;
      }
      const angle = Math.atan2(rxTop - txTop, right - left);
      const centerX = (left + right) / 2;
      const centerY = (txTop + rxTop) / 2;
      const linkLength = Math.hypot(right - left, rxTop - txTop);
      const zoneHeight = Math.max(22, Math.min(58, 20 + fresnelRadiusMeters * 1.7));

      context.save();
      context.translate(centerX, centerY);
      context.rotate(angle);
      const zoneGradient = context.createLinearGradient(0, -zoneHeight, 0, zoneHeight);
      zoneGradient.addColorStop(0, "rgba(45, 212, 191, 0.01)");
      zoneGradient.addColorStop(0.5, "rgba(45, 212, 191, 0.14)");
      zoneGradient.addColorStop(1, "rgba(45, 212, 191, 0.01)");
      context.beginPath();
      context.ellipse(0, 0, linkLength / 2, zoneHeight, 0, 0, Math.PI * 2);
      context.fillStyle = zoneGradient;
      context.fill();
      context.setLineDash([6, 5]);
      context.strokeStyle = blocked ? "rgba(251, 113, 133, 0.7)" : "rgba(45, 212, 191, 0.72)";
      context.lineWidth = 1.4;
      context.stroke();
      context.restore();

      context.setLineDash([]);
      const beamGradient = context.createLinearGradient(left, txTop, right, rxTop);
      beamGradient.addColorStop(0, "#55c8ff");
      beamGradient.addColorStop(0.55, statusColor);
      beamGradient.addColorStop(1, "#2dd4bf");
      context.strokeStyle = beamGradient;
      context.lineWidth = 2.2;
      context.shadowColor = statusColor;
      context.shadowBlur = 10;
      context.beginPath();
      context.moveTo(left, txTop);
      context.lineTo(right, rxTop);
      context.stroke();
      context.shadowBlur = 0;

      const horizonProgress = Math.min(1, horizonKm / Math.max(distanceKm, 0.01));
      if (horizonProgress < 1) {
        const horizonX = left + (right - left) * horizonProgress;
        context.setLineDash([4, 4]);
        context.strokeStyle = "rgba(251, 191, 36, 0.78)";
        context.beginPath();
        context.moveTo(horizonX, top + 18);
        context.lineTo(horizonX, groundBase + 18);
        context.stroke();
        context.setLineDash([]);
        context.fillStyle = "#fbbf24";
        context.font = "650 10px ui-monospace, monospace";
        context.textAlign = "center";
        context.fillText("RADIO HORIZON", horizonX, top + 10);
      }

      if (blocked) {
        const obstructionY = terrainY(0.5);
        context.beginPath();
        context.moveTo(centerX - 18, obstructionY);
        context.lineTo(centerX, centerY + 4);
        context.lineTo(centerX + 18, obstructionY);
        context.closePath();
        context.fillStyle = "rgba(251, 113, 133, 0.86)";
        context.fill();
        context.strokeStyle = "#ff9aac";
        context.stroke();
      }

      const drawEndpointLabel = (x: number, ground: number, label: string, color: string, feet: number) => {
        context.font = "750 12px ui-monospace, monospace";
        context.textAlign = "center";
        context.fillStyle = color;
        context.fillText(label, x, ground + 17);
        context.fillStyle = "#7890a3";
        context.font = "600 10px ui-monospace, monospace";
        context.fillText(`${feet.toFixed(0)} ft`, x, ground + 29);
      };

      const drawGroundStation = (
        x: number,
        ground: number,
        antennaTop: number,
        direction: -1 | 1,
        antenna: AntennaKind,
        color: string,
      ) => {
        if (environment.includes("Urban")) {
          const buildingHeight = ground - antennaTop - 8;
          const buildingWidth = environment.includes("Raised") ? 34 : 42;
          context.fillStyle = "rgba(36, 57, 72, .98)";
          context.fillRect(x - buildingWidth / 2, ground - buildingHeight, buildingWidth, buildingHeight);
          context.strokeStyle = "rgba(102, 139, 159, .52)";
          context.strokeRect(x - buildingWidth / 2, ground - buildingHeight, buildingWidth, buildingHeight);
          context.fillStyle = "rgba(251, 191, 36, .36)";
          context.fillRect(x - 10, ground - buildingHeight + 10, 5, 4);
          context.fillRect(x + 5, ground - buildingHeight + 10, 5, 4);
          context.strokeStyle = color;
          context.beginPath();
          context.moveTo(x, ground - buildingHeight);
          context.lineTo(x, antennaTop);
          context.stroke();
        } else {
          drawLatticeTower(context, x, ground, antennaTop, color);
        }
        drawAntenna(context, x, antennaTop - 2, direction, antenna, color);
      };

      if (environment === "Air to Ground" || environment === "Air to Air") {
        context.save();
        context.setLineDash([3, 5]);
        context.strokeStyle = "rgba(85, 200, 255, .24)";
        context.lineWidth = 1;
        context.beginPath();
        context.moveTo(left, txTop + 3);
        context.lineTo(left, txGround);
        if (environment === "Air to Air") {
          context.moveTo(right, rxTop + 3);
          context.lineTo(right, rxGround);
        }
        context.stroke();
        context.restore();
        drawAircraft(context, left - 9, txTop - 17, "#55c8ff");
        drawVentralBladeAntenna(context, left, txTop, "#55c8ff");
        if (environment === "Air to Air") {
          drawAircraft(context, right + 9, rxTop - 17, "#2dd4bf", -1);
          drawVentralBladeAntenna(context, right, rxTop, "#2dd4bf");
        } else {
          drawGroundStation(right, rxGround, rxTop, -1, scene.antennaRx, "#2dd4bf");
        }
      } else if (environment === "Maritime") {
        drawShip(context, left, txGround, "#55c8ff");
        drawShip(context, right, rxGround, "#2dd4bf");
        context.strokeStyle = "#55c8ff";
        context.beginPath();
        context.moveTo(left, txGround - 18);
        context.lineTo(left, txTop);
        context.stroke();
        context.strokeStyle = "#2dd4bf";
        context.beginPath();
        context.moveTo(right, rxGround - 18);
        context.lineTo(right, rxTop);
        context.stroke();
        drawAntenna(context, left, txTop, 1, scene.antennaTx, "#55c8ff");
        drawAntenna(context, right, rxTop, -1, scene.antennaRx, "#2dd4bf");
      } else if (environment === "Ground Robotics") {
        drawRover(context, left, txGround, "#55c8ff");
        drawRover(context, right, rxGround, "#2dd4bf");
        context.strokeStyle = "#55c8ff";
        context.beginPath();
        context.moveTo(left, txGround - 18);
        context.lineTo(left, txTop);
        context.stroke();
        context.strokeStyle = "#2dd4bf";
        context.beginPath();
        context.moveTo(right, rxGround - 18);
        context.lineTo(right, rxTop);
        context.stroke();
        drawAntenna(context, left, txTop, 1, scene.antennaTx, "#55c8ff");
        drawAntenna(context, right, rxTop, -1, scene.antennaRx, "#2dd4bf");
      } else {
        drawGroundStation(left, txGround, txTop, 1, scene.antennaTx, "#55c8ff");
        drawGroundStation(right, rxGround, rxTop, -1, scene.antennaRx, "#2dd4bf");
      }

      drawEndpointLabel(left, txGround, "TX", "#55c8ff", txHeightFeet);
      drawEndpointLabel(right, rxGround, "RX", "#2dd4bf", rxHeightFeet);

      const distanceLabel = `${distanceKm.toFixed(distanceKm < 10 ? 1 : 0)} km target path`;
      context.font = "700 11px ui-monospace, monospace";
      const labelWidth = context.measureText(distanceLabel).width + 20;
      roundedRect(context, centerX - labelWidth / 2, height - 31, labelWidth, 20, 5);
      context.fillStyle = "rgba(7, 17, 29, 0.92)";
      context.fill();
      context.strokeStyle = "rgba(85, 200, 255, 0.28)";
      context.stroke();
      context.fillStyle = "#b8cddd";
      context.textAlign = "center";
      context.fillText(distanceLabel, centerX, height - 17);

      context.fillStyle = blocked ? "#fb7185" : "#6ee7d5";
      context.font = "650 10px ui-monospace, monospace";
      context.textAlign = "center";
      context.fillText(
        blocked ? "60% FRESNEL CORRIDOR OBSTRUCTED" : `60% FRESNEL CLEARANCE · ${(fresnelRadiusMeters * 0.6).toFixed(1)} m`,
        centerX,
        Math.max(top + 16, centerY - zoneHeight - 10),
      );
    };

    draw();
    const resizeObserver = new ResizeObserver(draw);
    resizeObserver.observe(canvas);
    return () => resizeObserver.disconnect();
  }, [blocked, distanceKm, environment, fresnelRadiusMeters, horizonKm, rxHeightFeet, status, txHeightFeet]);

  return (
    <section className={`mimo-map-card${status === "No viable mode" ? " mimo-map-card--invalid" : ""}`}>
      <div className="mimo-map-heading">
        <div>
          <span className="section-kicker">Live geometry</span>
          <h2>Visual path map</h2>
          <p>Schematic terrain profile and radio corridor—not a geographic survey.</p>
        </div>
        <div className="mimo-map-readouts">
          <span>{environment}</span>
          <strong className={`map-status map-status--${status === "Reliable" ? "good" : status === "Possible" ? "moderate" : "weak"}`}>{status}</strong>
        </div>
      </div>
      <div className="mimo-map-equipment" aria-label="Environment-derived antenna profiles">
        <span><i className="equipment-mark equipment-mark--tx">TX</i> {scene.txLabel}</span>
        <span><i className="equipment-mark equipment-mark--rx">RX</i> {scene.rxLabel}</span>
      </div>
      <div className="mimo-map-stage">
        <canvas
          aria-label={`Visual link-path map for a ${distanceKm} kilometer ${environment} path using TX ${scene.txLabel} and RX ${scene.rxLabel}. ${status}. Fresnel zone is ${blocked ? "obstructed" : "clear"}.`}
          className="mimo-map-canvas"
          ref={canvasRef}
          role="img"
        >
          Visual link path from transmitter to receiver.
        </canvas>
      </div>
      <div className="mimo-map-legend" aria-label="Map legend">
        <span><i className="map-line map-line--path" />Radio path</span>
        <span><i className="map-line map-line--fresnel" />First Fresnel corridor</span>
        <span><i className="map-line map-line--terrain" />Modeled terrain</span>
        {blocked ? <span><i className="map-line map-line--obstruction" />Obstruction</span> : null}
      </div>
    </section>
  );
}
