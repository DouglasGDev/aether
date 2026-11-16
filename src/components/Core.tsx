export type CorePhase =
  | "dormant"
  | "ready"
  | "listening"
  | "working"
  | "awaiting"
  | "fault";

interface Props {
  phase: CorePhase;
}

export function Core({ phase }: Props) {
  return (
    <div className={`core core-${phase}`} aria-hidden="true">
      <svg viewBox="0 0 200 200" className="core-svg">
        <circle className="core-halo" cx="100" cy="100" r="92" />
        <circle className="core-ring ring-a" cx="100" cy="100" r="80" />
        <circle className="core-ring ring-b" cx="100" cy="100" r="64" />
        <circle className="core-ring ring-c" cx="100" cy="100" r="48" />
        <circle className="core-heart" cx="100" cy="100" r="24" />
        <circle className="core-pip" cx="100" cy="100" r="7" />
      </svg>
    </div>
  );
}