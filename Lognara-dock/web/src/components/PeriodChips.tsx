const day = new Intl.DateTimeFormat("ru-RU", { day: "2-digit", month: "2-digit" });
const minute = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", hour12: false });
const second = new Intl.DateTimeFormat("ru-RU", { second: "2-digit" });
const clock = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });

function Moment({ value }: { value: string }) {
  if (!Number.isFinite(Date.parse(value))) return <span className="period-moment">Некорректное время</span>;
  const date = new Date(value);
  return <time className="period-moment" dateTime={value} title={value}>
    <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="3" y="4" width="18" height="17" rx="3" /><path d="M3 9h18M8 2v4M16 2v4" /></svg>
    <span>{day.format(date)}</span>
    <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></svg>
    <span>{minute.format(date)}<small>:{second.format(date).padStart(2, "0")}</small></span>
  </time>;
}

/** Границы периода, часовой пояс и момент снимка. */
export default function PeriodChips({ from, to, asOf }: { from: string; to: string; asOf?: string }) {
  return <div className="period-chips" role="group" aria-label="Границы периода">
    <span className="muted">Период</span>
    <Moment value={from} /><span aria-hidden="true" className="muted">→</span><Moment value={to} />
    <span className="period-zone">{Intl.DateTimeFormat().resolvedOptions().timeZone}</span>
    {asOf && Number.isFinite(Date.parse(asOf)) &&
      <span className="period-fresh">Обновлено <time dateTime={asOf} title={asOf}>{clock.format(new Date(asOf))}</time></span>}
  </div>;
}
