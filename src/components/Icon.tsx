const PATHS: Record<string, string> = {
  eye: "M2 12s3.6-6 10-6 10 6 10 6-3.6 6-10 6-10-6-10-6Zm10 2.6A2.6 2.6 0 1 0 12 9.4a2.6 2.6 0 0 0 0 5.2Z",
  pencil: "M4 20h4l10-10-4-4L4 16v4Zm12.7-13.3 1.6-1.6a1.5 1.5 0 0 0-2.1-2.1l-1.6 1.6 2.1 2.1Z",
  warning: "M12 3 1.5 21h21L12 3Zm0 6v6m0 3v.5",
  shield: "M12 2 4 5.5v6c0 5 3.4 9 8 10.5 4.6-1.5 8-5.5 8-10.5v-6L12 2Z",
  check: "m4 12.5 5 5L20 6.5",
  cross: "M5 5l14 14M19 5 5 19",
  plus: "M12 5v14M5 12h14",
  send: "M3 11.5 21 3l-8.5 18-2.4-7.1L3 11.5Z",
  plug: "M9 2v6m6-6v6M6 8h12v3a6 6 0 0 1-6 6 6 6 0 0 1-6-6V8Zm6 9v5",
  spark: "M12 2.5 14 9l6.5 2-6.5 2-2 6.5-2-6.5L3.5 11 10 9l2-6.5Z",
  lock: "M6 11V8a6 6 0 0 1 12 0v3M5 11h14v10H5V11Z",
  bolt: "M13 2 4 14h6l-1 8 9-12h-6l1-8Z",
  download: "M12 3v12m0 0 4.5-4.5M12 15l-4.5-4.5M4 19h16",
  search: "M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13Zm4.8 1.3L20 21",
  trash: "M4 7h16M9 7V4h6v3m-8 0 1 14h8l1-14",
  cube: "M12 2.5 21 7v10l-9 4.5L3 17V7l9-4.5Zm0 0v19M3 7l9 4.5L21 7",
  file: "M6 2h8l4 4v16H6V2Zm8 0v4h4M9 12h6M9 16h6",
  folder: "M3 6h6l2 2.5h10V20H3V6Z",
  terminal:
    "M3 4h18v16H3V4Zm3.5 4L9 10.5 6.5 12m4 1.5H14",
  cpu: "M7 7h10v10H7V7Zm3 3h4v4h-4v-4ZM9 3v4m6-4v4M9 17v4m6-4v4M3 9h4m-4 6h4m10-6h4m-4 6h4",
  arrow: "M4 12h14m-4-4 4 4-4 4",
  clock: "M12 7v5l3 2M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18Z",
  globe:
    "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM3.2 9h17.6M3.2 15h17.6M12 3c-2.4 2.4-3.6 5.4-3.6 9s1.2 6.6 3.6 9c2.4-2.4 3.6-5.4 3.6-9s-1.2-6.6-3.6-9Z",
  nodes:
    "M5.5 3.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5Zm13 0a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5Zm-6.5 12a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5ZM7.6 6.8 10.4 15M16.4 6.8 13.6 15M8 6h8",
  refresh: "M20 12a8 8 0 1 1-2.4-5.7M20.2 4v4h-4",
  mic: "M12 3.6a2.7 2.7 0 0 1 2.7 2.7v5.3a2.7 2.7 0 0 1-5.4 0V6.3A2.7 2.7 0 0 1 12 3.6ZM5.8 11a6.2 6.2 0 0 0 12.4 0M12 17.4V21M9 21h6",
  speaker: "M4 9.5h3L12 6v12l-5-3.5H4v-5Zm11.5 2.5a3.2 3.2 0 0 1 0 3m2-7a6.4 6.4 0 0 1 0 11",
  play: "M8 5.6 18.5 12 8 18.4V5.6Z",
  stop: "M7 7h10v10H7Z",
  wave: "M4 10.5v3M8 7.5v9M12 4.5v15M16 8v8M20 10.5v3",
  escalas:
    "M3 7h18M3 12h18M3 17h18M11.5 7a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0ZM17.5 12a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0ZM9.5 17a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0Z",
  memoria:
    "M12 9.2a2.8 2.8 0 1 0 0 5.6 2.8 2.8 0 0 0 0-5.6ZM4 5.2h2.4v2.4H4zM17.6 5.2H20v2.4h-2.4zM4 16.4h2.4v2.4H4zM17.6 16.4H20v2.4h-2.4zM6.4 7.6l3.2 2.8M17.6 7.6l-3.2 2.8M6.4 16.4l3.2-2.8M17.6 16.4l-3.2-2.8",
  chevron: "m6 9 6 6 6-6",
  copiar: "M9 9.5h9.5V19H9V9.5ZM15.5 9.5V5H6v9.5h3",
  camadas:
    "M12 3 3.5 7.2 12 11.4l8.5-4.2L12 3ZM3.5 12 12 16.2l8.5-4.2M3.5 16.8 12 21l8.5-4.2",
  conversa:
    "M4 4.5h16v11.5H10.5L6 19.5v-3.5H4V4.5ZM8 8.5h8M8 12h5",
  bancada:
    "M5 3.5h14v17H5v-17Zm3 4.5 1.8 1.8 3.2-3.3m-5 6.5 1.8 1.8 3.2-3.3",
};

interface Props {
  name: string;
  className?: string;
}

export function Icon({ name, className }: Props) {
  const path = PATHS[name] ?? PATHS.spark;
  const filled = ["eye", "pencil", "shield", "send", "spark", "bolt", "play", "stop"].includes(name);
  return (
    <svg
      viewBox="0 0 24 24"
      className={className ? `icon ${className}` : "icon"}
      aria-hidden="true"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth={filled ? 0 : 1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d={path} />
    </svg>
  );
}