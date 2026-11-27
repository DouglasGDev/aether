interface Props {
  linhas?: number;
}

export function Esqueleto({ linhas = 3 }: Props) {
  return (
    <div className="skeleton" aria-hidden="true">
      {Array.from({ length: linhas }, (_, i) => (
        <span key={i} className="skeleton-row" />
      ))}
    </div>
  );
}
