"use client";

import { useMemo, useState, type ReactNode } from "react";
import LinkMap from "./LinkMap";
import MultiNodePlanner from "./MultiNodePlanner";
import {
  AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
  calculateMimo,
  DEFAULT_MIMO_INPUTS,
  ENVIRONMENTS,
  MAX_TX_HEIGHT_FEET,
  RADIO_PROFILES,
  type CrossPolarization,
  type EnvironmentKey,
  type MimoInputs,
  type PowerControl,
  type RadioProfileKey,
} from "./model";

type NumericKey = {
  [K in keyof MimoInputs]: MimoInputs[K] extends number ? K : never;
}[keyof MimoInputs];

function NumericControl({
  label,
  field,
  unit,
  min,
  max,
  step,
  value,
  onChange,
}: {
  label: string;
  field: NumericKey;
  unit: string;
  min: number;
  max: number;
  step: number;
  value: number;
  onChange: (field: NumericKey, value: number) => void;
}) {
  const id = `mimo-${field}`;
  return (
    <div className="mimo-control">
      <div className="mimo-control__line">
        <label htmlFor={id}>{label}</label>
        <div className="mimo-value-input">
          <input
            aria-label={`${label} in ${unit}`}
            max={max}
            min={min}
            onChange={(event) => {
              const next = Number(event.target.value);
              if (Number.isFinite(next)) onChange(field, Math.min(max, Math.max(min, next)));
            }}
            step={step}
            type="number"
            value={value}
          />
          <span>{unit}</span>
        </div>
      </div>
      <input
        id={id}
        max={max}
        min={min}
        onChange={(event) => onChange(field, Number(event.target.value))}
        step={step}
        type="range"
        value={value}
      />
    </div>
  );
}

function Toggle({
  checked,
  label,
  note,
  onChange,
}: {
  checked: boolean;
  label: string;
  note: string;
  onChange: (value: boolean) => void;
}) {
  return (
    <label className="mimo-toggle">
      <span><strong>{label}</strong><small>{note}</small></span>
      <input checked={checked} onChange={(event) => onChange(event.target.checked)} type="checkbox" />
      <i aria-hidden="true" />
    </label>
  );
}

function SelectControl({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: string[];
  onChange: (value: string) => void;
}) {
  return (
    <label className="mimo-select">
      <span>{label}</span>
      <select onChange={(event) => onChange(event.target.value)} value={value}>
        {options.map((option) => <option key={option}>{option}</option>)}
      </select>
    </label>
  );
}

function InputCard({
  number,
  title,
  note,
  summary,
  children,
}: {
  number?: string;
  title: string;
  note: string;
  summary?: ReactNode;
  children: ReactNode;
}) {
  const className = [
    "mimo-input-card",
    summary ? "" : "mimo-input-card--plain",
    number ? "" : "mimo-input-card--unnumbered",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <details className={className}>
      <summary className="mimo-card-heading">
        {number ? <span>{number}</span> : null}
        <div className="mimo-card-heading__copy"><h2>{title}</h2><p>{note}</p></div>
        {summary ? <div className="mimo-card-summary">{summary}</div> : null}
        <i aria-hidden="true" className="mimo-card-toggle" />
      </summary>
      <div className="mimo-input-card__body">{children}</div>
    </details>
  );
}

function SummaryPill({ children, tone }: { children: ReactNode; tone: "blue" | "green" | "amber" }) {
  return <span className={`mimo-summary-pill mimo-summary-pill--${tone}`}>{children}</span>;
}

const VARIABLE_DEFINITION_GROUPS = [
  {
    title: "Propagation",
    items: [
      ["Environment", "The propagation setting used to choose the path-loss exponent and scene assumptions."],
      ["Center frequency", "The radio carrier frequency used for free-space loss and Fresnel-zone calculations."],
      ["Target distance", "The planned separation between the transmitting and receiving sites."],
      ["Safety margin", "Extra link budget reserved for fading, interference, installation variation, and uncertainty."],
    ],
  },
  {
    title: "Transmit chain",
    items: [
      ["Radio profile", "The hardware family and documented RF limits used to constrain the planning estimate."],
      ["Maximum TX power", "The highest RF power available at the transmitter before cable and antenna effects."],
      ["Power control", "Adaptive reduces output when full power is unnecessary; Fixed retains the selected maximum."],
      ["TX cable loss", "Signal power lost between the transmitter and its antenna."],
      ["TX antenna gain", "Directional gain contributed by the transmitting antenna."],
      ["PA / BDA gain", "Additional transmit gain supplied by an enabled external amplifier."],
    ],
  },
  {
    title: "Receive chain",
    items: [
      ["Connected RX antennas", "The number of active receive antenna paths available for spatial streams and combining."],
      ["RX cable loss", "Signal power lost between the receiving antenna and radio input."],
      ["RX antenna gain", "Directional gain contributed by the receiving antenna."],
      ["RX noise figure", "Receiver-added noise above the theoretical thermal noise floor."],
      ["Cross-polarized antennas", "Whether antenna geometry supports independent spatial streams on one or both ends."],
    ],
  },
  {
    title: "Geometry & load",
    items: [
      ["TX antenna height", "Height of the transmitting antenna above local ground level."],
      ["RX antenna height", "Height of the receiving antenna above local ground level."],
      ["User data traffic", "Net payload throughput the link is expected to carry."],
      ["Fresnel-zone obstruction", "Applies an obstruction penalty when objects intrude into the inner 60% of the first Fresnel zone."],
    ],
  },
] as const;

export default function RfPlanningSuite() {
  const [plannerMode, setPlannerMode] = useState<"single" | "network">("single");
  const [inputs, setInputs] = useState<MimoInputs>(DEFAULT_MIMO_INPUTS);
  const results = useMemo(() => calculateMimo(inputs), [inputs]);
  const best = results.bestMode;
  const profile = RADIO_PROFILES[inputs.radioProfile];
  const statusTone = results.overallStatus === "Reliable" ? "good" : results.overallStatus === "Possible" ? "moderate" : "weak";

  const setNumeric = (field: NumericKey, value: number) => {
    setInputs((current) => ({ ...current, [field]: value }));
  };

  const applyRadioProfile = (radioProfile: RadioProfileKey) => {
    const nextProfile = RADIO_PROFILES[radioProfile];
    setInputs((current) => {
      const frequencySupported = nextProfile.supportedRangesMHz.some(([minimum, maximum]) => current.frequencyMHz >= minimum && current.frequencyMHz <= maximum);
      return {
        ...current,
        radioProfile,
        txPowerW: nextProfile.maxTxPowerW,
        rxAntennas: nextProfile.receivePaths,
        bdaUsed: false,
        frequencyMHz: frequencySupported ? current.frequencyMHz : nextProfile.bands[0].frequency,
      };
    });
  };

  const applyEnvironment = (environment: EnvironmentKey) => {
    setInputs((current) => environment === "Air to Air"
      ? {
          ...current,
          environment,
          fresnelBlocked: false,
          rxHeightFeet: AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
          txHeightFeet: AIR_TO_AIR_DEFAULT_ALTITUDE_FEET,
        }
      : { ...current, environment });
  };

  return (
    <main className="mimo-shell">
      <header className="mimo-header">
        <div className="mimo-header__main">
          <div>
            <div className="eyebrow"><span className="eyebrow__pulse" />MN-MIMO planning lab</div>
            <h1><span>4000 Series + SL5200</span> RF Planning Suite</h1>
            <p>Plan single-link performance or model a mixed-radio network with as many as 150 nodes.</p>
          </div>
          {plannerMode === "single" ? <button className="reset-button" onClick={() => setInputs(DEFAULT_MIMO_INPUTS)} type="button">Reset link defaults</button> : null}
        </div>
      </header>

      <nav className="planner-mode-switch" aria-label="Calculator mode">
        <button aria-pressed={plannerMode === "single"} onClick={() => setPlannerMode("single")} type="button"><span>Single link</span><small>Range, capacity, and geometry</small></button>
        <button aria-pressed={plannerMode === "network"} onClick={() => setPlannerMode("network")} type="button"><span>Multi-node network</span><small>Up to 150 mixed-radio nodes</small></button>
      </nav>

      {plannerMode === "single" ? <>

      <section className={`mimo-status mimo-status--${statusTone}`} aria-live="polite">
        <div className="mimo-status__title">
          <span>Scenario assessment</span>
          <h2>{results.overallStatus}</h2>
          <p>{best ? `${best.bandwidth} MHz · ${best.nss} spatial stream${best.nss === 2 ? "s" : ""} is the strongest available mode.` : "No operating mode satisfies the current path."}</p>
        </div>
        <div className="mimo-status__recommendation">
          <span>Recommendation</span>
          <h3>{!results.frequencySupported ? "Choose a supported radio band" : best?.reliable ? "Keep this operating point" : best?.viable ? "Add margin before deployment" : "Revise the link design"}</h3>
          <p>{!results.frequencySupported ? `${profile.label} does not support ${inputs.frequencyMHz} MHz. Choose one of its band presets.` : best?.reliable ? `The ${best.bandwidth} MHz ${best.nss}SS mode carries ${inputs.userTrafficMbps.toFixed(1)} Mbps with ${best.snr.toFixed(1)} dB SNR and ${best.dutyCycle?.toFixed(0)}% air time.` : best?.viable ? "The link closes, but it is outside the preferred SNR or air-time region. Reduce traffic, narrow bandwidth, improve antennas, or shorten the path." : "No enabled mode closes at the requested distance and traffic. Increase link gain, reduce losses, narrow the path, or lower the traffic target."}</p>
        </div>
        <div className="mimo-status__numbers">
          <div><span>Best capacity</span><strong>{best?.capacity?.toFixed(1) ?? "—"}</strong><small>Mbps</small></div>
          <div><span>Mode SNR</span><strong>{best?.snr.toFixed(1) ?? "—"}</strong><small>dB</small></div>
          <div><span>Air time</span><strong>{best?.dutyCycle?.toFixed(0) ?? "—"}</strong><small>%</small></div>
        </div>
      </section>

      <div className="mimo-layout">
        <aside className="mimo-inputs">
          <InputCard
            note="Environment and carrier"
            number="01"
            summary={<>
              <SummaryPill tone="blue">{inputs.environment}</SummaryPill>
              <SummaryPill tone="green">{inputs.frequencyMHz} MHz</SummaryPill>
              <SummaryPill tone="amber">{inputs.targetDistanceKm} km</SummaryPill>
            </>}
            title="Propagation"
          >
            <SelectControl
              label="Environment"
              onChange={(value) => applyEnvironment(value as EnvironmentKey)}
              options={Object.keys(ENVIRONMENTS)}
              value={inputs.environment}
            />
            <div className="environment-note"><span>n = {results.environment.exponent.toFixed(1)}</span>{results.environment.note}</div>
            <div className="mimo-band-picker" aria-label="Quick radio-band presets">
              <span>Quick radio bands</span>
              <div>
              {profile.bands.map((band) => (
                  <button
                    aria-pressed={inputs.frequencyMHz === band.frequency}
                    className={inputs.frequencyMHz === band.frequency ? "is-active" : ""}
                    key={band.label}
                    onClick={() => setNumeric("frequencyMHz", band.frequency)}
                    type="button"
                  >
                    <strong>{band.label}</strong>
                    <small>{band.display}</small>
                  </button>
                ))}
              </div>
            </div>
            <NumericControl field="frequencyMHz" label="Center frequency" max={6000} min={400} onChange={setNumeric} step={10} unit="MHz" value={inputs.frequencyMHz} />
            <NumericControl field="targetDistanceKm" label="Target distance" max={100} min={0.1} onChange={setNumeric} step={0.1} unit="km" value={inputs.targetDistanceKm} />
            <NumericControl field="safetyMargin" label="Safety margin" max={30} min={0} onChange={setNumeric} step={1} unit="dB" value={inputs.safetyMargin} />
          </InputCard>

          <InputCard
            note="Power and antenna system"
            number="02"
            summary={<>
              <SummaryPill tone="blue">{profile.shortLabel}</SummaryPill>
              <SummaryPill tone="green">{inputs.txPowerW} W</SummaryPill>
              <SummaryPill tone="amber">{inputs.powerControl}</SummaryPill>
            </>}
            title="Transmit chain"
          >
            <label className="mimo-select">
              <span>Radio profile</span>
              <select onChange={(event) => applyRadioProfile(event.target.value as RadioProfileKey)} value={inputs.radioProfile}>
                {(Object.keys(RADIO_PROFILES) as RadioProfileKey[]).map((key) => <option key={key} value={key}>{RADIO_PROFILES[key].label}</option>)}
              </select>
            </label>
            <div className={`radio-profile-note${profile.estimated ? " radio-profile-note--estimated" : ""}`}><strong>{profile.label}</strong><span>{profile.note}</span>{profile.estimated ? <em>Supported: L 1350–1440 · S 2200–2500 · C 4400–5000 MHz</em> : null}</div>
            <NumericControl field="txPowerW" label="Maximum native TX power" max={profile.maxTxPowerW} min={0.001} onChange={setNumeric} step={0.1} unit="W" value={inputs.txPowerW} />
            <SelectControl
              label="Power control"
              onChange={(value) => setInputs((current) => ({ ...current, powerControl: value as PowerControl }))}
              options={["Adaptive", "Fixed"]}
              value={inputs.powerControl}
            />
            <NumericControl field="txCableLoss" label="TX cable loss" max={10} min={0} onChange={setNumeric} step={0.1} unit="dB" value={inputs.txCableLoss} />
            <NumericControl field="txAntennaGain" label="TX antenna gain" max={35} min={0} onChange={setNumeric} step={0.5} unit="dBi" value={inputs.txAntennaGain} />
            <Toggle checked={inputs.bdaUsed} label="Bidirectional amplifier" note="Apply external PA gain" onChange={(value) => setInputs((current) => ({ ...current, bdaUsed: value }))} />
            {inputs.bdaUsed ? <NumericControl field="paGain" label="PA / BDA gain" max={35} min={0} onChange={setNumeric} step={0.5} unit="dB" value={inputs.paGain} /> : null}
          </InputCard>

          <InputCard
            note="Sensitivity and antenna system"
            number="03"
            summary={<>
              <SummaryPill tone="blue">{inputs.rxAntennas} antennas</SummaryPill>
              <SummaryPill tone="green">{inputs.rxAntennaGain} dBi</SummaryPill>
              <SummaryPill tone="amber">NF {inputs.noiseFigure} dB</SummaryPill>
            </>}
            title="Receive chain"
          >
            <SelectControl
              label="Connected RX antennas"
              onChange={(value) => setInputs((current) => ({ ...current, rxAntennas: Number(value) as 2 | 4 }))}
              options={profile.receivePaths === 2 ? ["2"] : ["4", "2"]}
              value={String(inputs.rxAntennas)}
            />
            <NumericControl field="rxCableLoss" label="RX cable loss" max={10} min={0} onChange={setNumeric} step={0.1} unit="dB" value={inputs.rxCableLoss} />
            <NumericControl field="rxAntennaGain" label="RX antenna gain" max={35} min={0} onChange={setNumeric} step={0.5} unit="dBi" value={inputs.rxAntennaGain} />
            <NumericControl field="noiseFigure" label="RX noise figure" max={15} min={1} onChange={setNumeric} step={0.5} unit="dB" value={inputs.noiseFigure} />
            <SelectControl
              label="Cross-polarized antennas"
              onChange={(value) => setInputs((current) => ({ ...current, crossPolarization: value as CrossPolarization }))}
              options={["Both Sides", "One Side", "No"]}
              value={inputs.crossPolarization}
            />
          </InputCard>

          <InputCard
            note="Clearance and traffic target"
            number="04"
            summary={<>
              <SummaryPill tone="blue">TX {inputs.txHeightFeet} ft</SummaryPill>
              <SummaryPill tone="green">RX {inputs.rxHeightFeet} ft</SummaryPill>
              <SummaryPill tone="amber">{inputs.userTrafficMbps} Mbps</SummaryPill>
            </>}
            title="Geometry & load"
          >
            <NumericControl field="txHeightFeet" label="TX antenna height" max={MAX_TX_HEIGHT_FEET} min={1} onChange={setNumeric} step={1} unit="ft" value={inputs.txHeightFeet} />
            <NumericControl field="rxHeightFeet" label="RX antenna height" max={MAX_TX_HEIGHT_FEET} min={1} onChange={setNumeric} step={1} unit="ft" value={inputs.rxHeightFeet} />
            <NumericControl field="userTrafficMbps" label="User data traffic" max={100} min={0.1} onChange={setNumeric} step={0.1} unit="Mbps" value={inputs.userTrafficMbps} />
            <Toggle checked={inputs.fresnelBlocked} label="Fresnel-zone obstruction" note="Objects inside the inner 60%" onChange={(value) => setInputs((current) => ({ ...current, fresnelBlocked: value }))} />
          </InputCard>

          <InputCard note="Reference glossary" title="Variable definitions">
            <div className="mimo-definition-groups">
              {VARIABLE_DEFINITION_GROUPS.map((group) => (
                <details className="mimo-definition-group" key={group.title}>
                  <summary>
                    <span>{group.title}</span>
                    <i aria-hidden="true" />
                  </summary>
                  <dl>
                    {group.items.map(([term, definition]) => (
                      <div className="mimo-definition-row" key={term}>
                        <dt>{term}</dt>
                        <dd>{definition}</dd>
                      </div>
                    ))}
                  </dl>
                </details>
              ))}
            </div>
          </InputCard>
        </aside>

        <section className="mimo-results">
          <div className="mimo-geometry-grid">
            <div className={results.horizonClear ? "mimo-geometry-card is-clear" : "mimo-geometry-card is-warning"}>
              <span>Radio horizon</span><strong>{results.horizonKm.toFixed(1)} km</strong><small>{results.horizonClear ? "Target is inside horizon" : "Raise one or both antennas"}</small>
              <div className="geometry-meter"><i style={{ width: `${Math.min(100, (results.horizonKm / inputs.targetDistanceKm) * 100)}%` }} /></div>
            </div>
            <div className="mimo-geometry-card">
              <span>First Fresnel zone</span><strong>{results.fresnelRadiusMeters.toFixed(1)} m</strong><small>{results.usableFresnelRadiusMeters.toFixed(1)} m clear radius recommended</small>
              <div className="fresnel-graphic"><i /><b /></div>
            </div>
            <div className="mimo-geometry-card">
              <span>Modeled path loss</span><strong>{results.pathLoss.toFixed(1)} dB</strong><small>Environment-adjusted at {inputs.targetDistanceKm.toFixed(1)} km</small>
              <div className="path-loss-dots" aria-hidden="true"><i /><i /><i /><i /><i /></div>
            </div>
          </div>

          <LinkMap
            blocked={inputs.fresnelBlocked}
            distanceKm={inputs.targetDistanceKm}
            environment={inputs.environment}
            fresnelRadiusMeters={results.fresnelRadiusMeters}
            horizonKm={results.horizonKm}
            rxHeightFeet={inputs.rxHeightFeet}
            status={results.overallStatus}
            txHeightFeet={inputs.txHeightFeet}
          />

          <div className="mimo-performance-card">
            <div className="mimo-results-heading">
              <div><span className="section-kicker">At target distance</span><h2>Mode performance</h2><p>SNR ≥ 5 dB and air time ≤ 80% is the recommended operating region.</p></div>
              <div className="mimo-legend"><span><i className="legend-good" />Reliable</span><span><i className="legend-moderate" />Possible</span><span><i className="legend-weak" />Invalid</span></div>
            </div>
            <div className="mimo-table-wrap">
              <table className="mimo-table">
                <thead><tr><th>Bandwidth</th><th>Streams</th><th>RSSI</th><th>SNR</th><th>MCS</th><th>Link capacity</th><th>Air time</th><th>Assessment</th></tr></thead>
                <tbody>
                  {results.modes.map((mode) => {
                    const tone = !mode.enabled || !mode.viable ? "weak" : mode.reliable ? "good" : "moderate";
                    return (
                      <tr className={`mode-${tone}`} key={`${mode.bandwidth}-${mode.nss}`}>
                        <td><strong>{mode.bandwidth}</strong> MHz</td>
                        <td><span className="stream-badge">{mode.nss}SS</span>{mode.nss === 2 && !mode.enabled ? <small>Locked</small> : null}</td>
                        <td>{mode.rssi.toFixed(1)} <small>dBm</small></td>
                        <td>{mode.snr.toFixed(1)} <small>dB</small></td>
                        <td>{mode.enabled && mode.mcs !== null ? <><strong>{mode.mcs}</strong><small>{mode.modulation}</small></> : "—"}</td>
                        <td>{mode.enabled && mode.capacity !== null ? <><strong>{mode.capacity.toFixed(2)}</strong> <small>Mbps</small></> : "—"}</td>
                        <td>{mode.enabled && mode.dutyCycle !== null ? <div className="duty-cell"><span>{mode.dutyCycle.toFixed(0)}%</span><i><b style={{ width: `${Math.min(100, mode.dutyCycle)}%` }} /></i></div> : "—"}</td>
                        <td><span className={`assessment assessment--${tone}`}>{!mode.enabled ? "Unavailable" : mode.reliable ? "Reliable" : mode.viable ? "Possible" : "Invalid"}</span></td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>

          <div className="mimo-guidance-grid">
            <details className="mimo-method-card"><summary>Model notes</summary><p>The 4000 Series profile reproduces the supplied workbook’s environment exponents, receiver sensitivities, adaptive-power backoff, MCS throughput table, Fresnel penalty, cross-polarization behavior, horizon calculation, and recommended SNR / duty-cycle limits. The SL5200 estimate applies its public 2 W, 2×2 MIMO, channel-bandwidth, sensitivity, and L/S/C band limits to the compatible MN-MIMO curve. Validate estimated results before deployment.</p></details>
          </div>
        </section>
      </div>
      </> : <MultiNodePlanner />}
    </main>
  );
}
