import type { Language } from "../preferences";

const BOX = { width: 20, height: 14, viewBox: "0 0 30 20", className: "shrink-0 rounded-[2px] ring-1 ring-black/10" };

function UnitedStates() {
  return (
    <svg {...BOX} aria-hidden>
      <rect width="30" height="20" fill="#fff" />
      {[0, 2, 4, 6, 8, 10, 12].map((i) => (
        <rect key={i} y={(i * 20) / 13} width="30" height={20 / 13} fill="#b22234" />
      ))}
      <rect width="12" height={(7 * 20) / 13} fill="#3c3b6e" />
    </svg>
  );
}

function Korea() {
  return (
    <svg {...BOX} aria-hidden>
      <rect width="30" height="20" fill="#fff" />
      <path d="M10 10a5 5 0 0 1 10 0a2.5 2.5 0 0 1-5 0a2.5 2.5 0 0 0-5 0Z" fill="#cd2e3a" />
      <path d="M20 10a5 5 0 0 1-10 0a2.5 2.5 0 0 1 5 0a2.5 2.5 0 0 0 5 0Z" fill="#0047a0" />
      <g stroke="#000" strokeWidth="1">
        <path d="M5 4.5l3-2M5.7 5.5l3-2M6.4 6.5l3-2" />
        <path d="M20.6 15.5l3-2M21.3 16.5l3-2M22 17.5l3-2" />
        <path d="M20.6 4.5l3 2M21.3 3.5l3 2M22 2.5l3 2" />
        <path d="M5 15.5l3 2M5.7 14.5l3 2M6.4 13.5l3 2" />
      </g>
    </svg>
  );
}

function China() {
  return (
    <svg {...BOX} aria-hidden>
      <rect width="30" height="20" fill="#de2910" />
      <path d="M5 2.5l1.18 3.6h3.8l-3.07 2.23 1.17 3.6L5 9.7l-3.08 2.23 1.17-3.6L0.02 6.1h3.8Z" fill="#ffde00" />
      {[
        [10, 2],
        [12, 4],
        [12, 7],
        [10, 9],
      ].map(([x, y]) => (
        <circle key={`${x}-${y}`} cx={x} cy={y} r="0.8" fill="#ffde00" />
      ))}
    </svg>
  );
}

function Brazil() {
  return (
    <svg {...BOX} aria-hidden>
      <rect width="30" height="20" fill="#009c3b" />
      <path d="M15 2L27 10L15 18L3 10Z" fill="#ffdf00" />
      <circle cx="15" cy="10" r="4.6" fill="#002776" />
      <path d="M10.6 9.2a9 9 0 0 1 8.8 2.2" stroke="#fff" strokeWidth="0.8" fill="none" />
    </svg>
  );
}

const FLAGS: Record<Language, () => React.JSX.Element> = {
  "en-US": UnitedStates,
  "ko-KR": Korea,
  "zh-CN": China,
  "pt-BR": Brazil,
};

export function Flag({ language }: { language: Language }) {
  const Component = FLAGS[language];
  return <Component />;
}
