import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Wrench } from "lucide-react";

import { agentApi, type SkillInfo, type ToolInfo, type WorkerReadiness } from "../api/agent";
import { Card } from "../components/Card";
import { Empty, Pill } from "../components/Bits";
import { ExtensionsCard } from "../components/Extensions";
import { McpCard } from "../components/Mcp";
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

/** A tool's permission in words: what a run's call does. */
const PERMISSION: Record<string, { label: string; tone: Tone }> = {
  auto: { label: "Runs without asking", tone: "ok" },
  ask: { label: "Asks first", tone: "info" },
  block: { label: "Blocked", tone: "danger" },
};

function skillSource(skill: SkillInfo): string | null {
  if (!skill.source) return null;
  if (skill.source.scope === "extension") return `Extension ${skill.source.extension ?? ""}`.trim();
  return skill.source.scope === "profile" ? "This profile" : "Every profile";
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
  const [includeMcp, setIncludeMcp] = useState(false);
  const [mcpState, setMcpState] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    setError(null);
    setSkills(null);
    setTools(null);
    Promise.all([agentApi.tools({ includeMcp }), agentApi.skills()])
      .then(([toolResponse, skillResponse]) => {
        if (live) {
          setTools(toolResponse.tools);
          setMcpState(toolResponse.mcp ?? null);
          setSkills(skillResponse.skills);
        }
      })
      .catch((cause) => {
        if (live) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      live = false;
    };
  }, [attempt, includeMcp]);

  return (
    <>
      <TopBar title="Tools" subtitle="What the agent can call, and how each is gated" />
      <div className="page">
        <Card title="Decision services">
          <JevToolCard />
        </Card>
        <Card title="Enabled tools">
          <p className="card__note">
            Permission is what a run&apos;s call does under this profile&apos;s approval policy, as lightagent tools
            list reports it.
          </p>
          {mcpState === "skipped" && (
            <div className="notice notice--info" role="status">
              MCP server tools are not listed: listing them connects every MCP server agent runs use, which starts
              their local commands.{" "}
              <button type="button" className="btn" onClick={() => setIncludeMcp(true)}>
                Include MCP server tools
              </button>
            </div>
          )}
          {mcpState === "connected" && (
            <div className="notice notice--info" role="status">
              MCP server tools are included: their servers were connected (local commands started) to list them.
            </div>
          )}
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
                  {tool.permission && (
                    <Pill tone={PERMISSION[tool.permission]?.tone ?? "neutral"}>
                      {PERMISSION[tool.permission]?.label ?? tool.permission}
                    </Pill>
                  )}
                  <span className="muted" style={{ fontSize: 13 }}>
                    {tool.description}
                  </span>
                </div>
              ))}
            </div>
          )}
        </Card>
        <ExtensionsCard />
        <McpCard />
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
                  {skillSource(skill) && <span style={{ marginLeft: 8 }}><Pill tone="info">{skillSource(skill)}</Pill></span>}
                  {skill.version && <span className="muted" style={{ marginLeft: 8, fontSize: 13 }}>{skill.version}</span>}
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
