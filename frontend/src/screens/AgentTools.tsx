import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Wrench } from "lucide-react";

import { agentApi, type SkillInfo, type ToolInfo, type WorkerReadiness } from "../api/agent";
import { Card } from "../components/Card";
import { Empty, Pill } from "../components/Bits";
import { JevToolCard } from "../components/Jev";
import { TopBar } from "../components/Shell";

type Tone = "ok" | "warn" | "danger" | "info" | "accent" | "neutral";

function riskTone(risk: string): Tone {
  switch (risk) {
    case "observe":
      return "ok";
    case "external":
      return "info";
    case "sensitive":
    case "mutating":
      return "warn";
    case "executable":
    case "privileged":
      return "danger";
    default:
      return "neutral";
  }
}

/** A readiness state in words and a tone; never colour alone. */
const READINESS: Record<WorkerReadiness["state"], { label: string; tone: Tone }> = {
  ready: { label: "Ready", tone: "ok" },
  degraded: { label: "Degraded", tone: "warn" },
  disabled: { label: "Disabled", tone: "neutral" },
  permission_denied: { label: "Not permitted", tone: "danger" },
  unavailable: { label: "Unavailable", tone: "neutral" },
};

function Specialists() {
  const [workers, setWorkers] = useState<WorkerReadiness[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    agentApi
      .workers()
      .then((response) => live && setWorkers(response.workers))
      .catch((cause) => live && setError(cause instanceof Error ? cause.message : String(cause)));
    return () => {
      live = false;
    };
  }, []);
  if (error) return <p className="muted">Specialist readiness is unavailable: {error}</p>;
  if (!workers) return <span className="muted">Loading specialists…</span>;
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {workers.map((worker) => {
        const state = READINESS[worker.state] ?? { label: worker.state, tone: "neutral" as Tone };
        return (
          <div key={worker.role} style={{ padding: "10px 12px", border: "1px solid var(--border, #2a2a2a)", borderRadius: 8 }}>
            <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
              <strong style={{ minWidth: 90, textTransform: "capitalize" }}>{worker.role}</strong>
              <Pill tone={state.tone}>{state.label}</Pill>
              <span className="muted" style={{ fontSize: 13 }}>
                {worker.worker}
                {worker.model && ` · ${worker.model}`}
                {worker.skill && ` · ${worker.skill}`}
              </span>
            </div>
            {worker.reasons.length > 0 && (
              <p className="muted" style={{ margin: "6px 0 0", fontSize: 12.5 }}>
                {worker.reasons.join(", ").replaceAll("_", " ")}
              </p>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function AgentTools() {
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  const [tools, setTools] = useState<ToolInfo[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let live = true;
    setError(null);
    setSkills(null);
    setTools(null);
    Promise.all([agentApi.tools(), agentApi.skills()])
      .then(([toolResponse, skillResponse]) => {
        if (live) {
          setTools(toolResponse.tools);
          setSkills(skillResponse.skills);
        }
      })
      .catch((cause) => {
        if (live) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      live = false;
    };
  }, [attempt]);

  return (
    <>
      <TopBar title="Tools" subtitle="What the agent can call, and how each is gated" />
      <div className="page">
        <Card title="Decision services">
          <JevToolCard />
        </Card>
        <Card title="Enabled tools">
          {error ? (
            <div role="alert">
              <p className="muted">Could not reach the agent API: {error}</p>
              <button type="button" className="btn" onClick={() => setAttempt((value) => value + 1)}>
                Retry
              </button>
              <Link className="btn" to="/settings" style={{ marginLeft: 8 }}>
                Server settings
              </Link>
            </div>
          ) : !tools ? (
            <span className="muted">Loading…</span>
          ) : tools.length === 0 ? (
            <Empty title="No tools" />
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              {tools.map((tool) => (
                <div
                  key={tool.name}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 10,
                    padding: "10px 12px",
                    border: "1px solid var(--border, #2a2a2a)",
                    borderRadius: 8,
                  }}
                >
                  <Wrench size={15} />
                  <strong style={{ minWidth: 140 }}>{tool.name}</strong>
                  <Pill tone={riskTone(tool.risk)}>{tool.risk}</Pill>
                  <span className="muted" style={{ fontSize: 13 }}>
                    {tool.description}
                  </span>
                </div>
              ))}
            </div>
          )}
        </Card>
        <Card title="Specialists">
          <p className="card__note">The roles a task can be delegated to, and whether each can do its work now. Every run rechecks.</p>
          <Specialists />
        </Card>
        <Card title="Installed skills">
          <p className="card__note">Instructions available to the harness on demand. Full skill bodies remain server-side.</p>
          {!skills ? <span className="muted">Loading skills…</span> : skills.length === 0 ? (
            <Empty title="No skills installed" hint="Add a SKILL.md under the global or active profile skills directory." />
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
              {skills.map((skill) => (
                <div key={skill.name} style={{ padding: "10px 12px", border: "1px solid var(--border, #2a2a2a)", borderRadius: 8 }}>
                  <strong>{skill.name}</strong>
                  {skill.description && <span className="muted" style={{ marginLeft: 10, fontSize: 13 }}>{skill.description}</span>}
                </div>
              ))}
            </div>
          )}
        </Card>
      </div>
    </>
  );
}
