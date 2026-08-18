# 4000 Series + SL5200 RF Planning Suite

A local, single-page RF planning application with:

- a workbook-compatible point-to-point MN-MIMO link budget;
- an estimated SL5200 profile calibrated to its public datasheet;
- mixed-radio network planning for as many as 150 nodes;
- direct, relay-chain, and repeatable random-branch topologies;
- packet-based and extensive traffic tests; and
- an interactive 3D network map.

## Run locally

Requires Node.js 22.13 or newer.

```bash
npm install
npm run dev
```

Use `npm test` to run a production build plus calculation and rendered-page regression tests.

## Calculation provenance

The 4000 Series single-link model reproduces the supplied workbook's:

- environment path-loss exponents;
- MCS sensitivities and UDP capacity tables for 20, 10, 5, 2.5, and 1.25 MHz;
- separate adaptive-power backoff curves for a native radio and a PA/BDA;
- two- versus four-receive-path sensitivity adjustment;
- 6 dB Fresnel-obstruction penalty;
- 3 dB one-side or both-sides cross-polarization penalty;
- two-spatial-stream availability only with cross-polarization on both sides;
- radio-horizon and Fresnel-radius equations; and
- possible/reliable limits of 0/5 dB SNR and 100/80 percent air time.

The default scenario is regression-tested against all cached workbook output modes to within 0.000001 dB.

The earlier pasted HTML used free-space loss plus a draggable single knife-edge obstacle. Its FSPL, first-Fresnel-radius, 4/3-Earth bulge, and ITU-style knife-edge equations were checked, but that obstacle model is not mixed into the workbook-derived calculator: the unified app intentionally uses the workbook's environment exponent and its fixed 6 dB Fresnel-obstruction assumption. Combining both would double-count obstruction loss.

## Extended-model assumptions

The original workbook is point-to-point and unidirectional. Network mode extends it conservatively:

- each hop is evaluated in both directions and the weaker result is used;
- downstream node traffic accumulates on upstream relay hops;
- incident-link duty cycles are summed at each half-duplex gateway/relay;
- more than 100 percent modeled radio airtime is overloaded, and more than 80 percent is marginal;
- altitude changes slant range and the radio horizon; and
- the average-distance input is the horizontal spacing of every modeled linked pair.

The SL5200 profile uses the public 2 W native-power, frequency-band, channel-bandwidth, and sensitivity anchors. Its detailed per-MCS range/capacity curve remains an estimate and must be validated with controlled data or field testing.

The visual maps are schematic. They do not import terrain, clutter, interference, antenna patterns, spectrum occupancy, or weather, and they are not a substitute for a site survey or mission-specific RF analysis.
