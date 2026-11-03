import { useId } from "react";

/** The spark of ognivo (огниво: a flint and steel), in the accent's colour. */
export function Logo({ size = 18, className = "" }: { size?: number; className?: string }) {
  const id = useId();
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" aria-hidden className={`shrink-0 ${className}`}>
      <defs>
        <radialGradient id={id} cx="50%" cy="50%" r="55%">
          <stop offset="0%" style={{ stopColor: "#fff8ec" }} />
          <stop offset="38%" style={{ stopColor: "var(--spark)" }} />
          <stop offset="100%" style={{ stopColor: "var(--spark)", stopOpacity: 0.25 }} />
        </radialGradient>
      </defs>
      <path d="M12 1.5 14.1 9.9 22.5 12 14.1 14.1 12 22.5 9.9 14.1 1.5 12 9.9 9.9Z" fill={`url(#${id})`} />
      <circle cx="19.4" cy="4.6" r="1.35" style={{ fill: "var(--spark)" }} opacity="0.85" />
      <circle cx="4.9" cy="19.1" r="0.85" style={{ fill: "var(--spark)" }} opacity="0.6" />
    </svg>
  );
}

/** The mark and the name. */
export function Wordmark() {
  return (
    <span className="flex items-center gap-2">
      <Logo size={18} />
      <span className="text-[15px] font-semibold tracking-tight text-spark">ognivo</span>
    </span>
  );
}
