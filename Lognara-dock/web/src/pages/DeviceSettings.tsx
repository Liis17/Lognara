import { useCallback, useEffect, useMemo, useState } from "react";
import qrcode from "qrcode-generator";
import { api, errorText, type DeviceInfo, type DevicePairing, type DevicePairingStatus } from "../api";

function formatTime(ms: number | null): string {
  return ms === null ? "не использовалось" : new Date(ms).toLocaleString("ru-RU");
}

function formatCountdown(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  const remainder = seconds % 60;
  return `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
}

function qrPath(payload: string): { viewBox: string; path: string } {
  const qr = qrcode(0, "M");
  qr.addData(payload);
  qr.make();

  const moduleCount = qr.getModuleCount();
  const quietZone = 4;
  const path: string[] = [];
  for (let row = 0; row < moduleCount; row += 1) {
    for (let column = 0; column < moduleCount; column += 1) {
      if (qr.isDark(row, column)) {
        path.push(`M${column + quietZone} ${row + quietZone}h1v1h-1z`);
      }
    }
  }

  const size = moduleCount + quietZone * 2;
  return { viewBox: `0 0 ${size} ${size}`, path: path.join("") };
}

function DeviceQr({ payload }: { payload: string }) {
  const qr = useMemo(() => qrPath(payload), [payload]);

  return (
    <svg className="device-qr" viewBox={qr.viewBox} role="img" aria-label="QR-код для подключения устройства" shapeRendering="crispEdges">
      <rect width="100%" height="100%" fill="#fff" />
      <path d={qr.path} fill="#171412" />
    </svg>
  );
}

type PairingResult = "idle" | DevicePairingStatus;

export default function DeviceSettings() {
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [listLoading, setListLoading] = useState(true);
  const [listError, setListError] = useState("");
  const [pairing, setPairing] = useState<DevicePairing | null>(null);
  const [pairingResult, setPairingResult] = useState<PairingResult>("idle");
  const [secondsLeft, setSecondsLeft] = useState(0);
  const [creating, setCreating] = useState(false);
  const [revokingId, setRevokingId] = useState<string | null>(null);
  const [manualCodeVisible, setManualCodeVisible] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState("");

  const refreshDevices = useCallback(async () => {
    setListLoading(true);
    setListError("");
    try {
      const reply = await api<{ devices: DeviceInfo[] }>("GET", "/api/devices");
      setDevices(reply.devices);
    } catch (e) {
      setListError(errorText(e));
    } finally {
      setListLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshDevices();
  }, [refreshDevices]);

  useEffect(() => {
    if (!pairing) return;
    const controller = new AbortController();
    let timer: number | undefined;

    const poll = async () => {
      try {
        const status = await api<{ status: DevicePairingStatus }>(
          "GET",
          `/api/devices/pairing/${encodeURIComponent(pairing.pairing_id)}`,
          undefined,
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;
        setError("");
        if (status.status === "pending") {
          timer = window.setTimeout(poll, 2_000);
          return;
        }
        setPairingResult(status.status);
        setPairing(null);
        setManualCodeVisible(false);
        if (status.status === "used") void refreshDevices();
      } catch (e) {
        if (controller.signal.aborted) return;
        setError(errorText(e));
        timer = window.setTimeout(poll, 2_000);
      }
    };

    timer = window.setTimeout(poll, 2_000);
    return () => {
      controller.abort();
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [pairing, refreshDevices]);

  useEffect(() => {
    if (!pairing) return;
    const updateCountdown = () => {
      const remaining = Math.max(0, Math.ceil((pairing.expires_at - Date.now()) / 1_000));
      setSecondsLeft(remaining);
      if (remaining === 0) {
        setPairingResult("expired");
        setPairing(null);
        setManualCodeVisible(false);
      }
    };

    updateCountdown();
    const timer = window.setInterval(updateCountdown, 1_000);
    return () => window.clearInterval(timer);
  }, [pairing]);

  const createPairing = async () => {
    setCreating(true);
    setError("");
    try {
      const reply = await api<DevicePairing>("POST", "/api/devices/pairing");
      setPairing(reply);
      setPairingResult("pending");
      setManualCodeVisible(false);
      setCopied(false);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setCreating(false);
    }
  };

  const copyManualCode = async () => {
    if (!pairing) return;
    try {
      await navigator.clipboard.writeText(pairing.payload);
      setCopied(true);
    } catch (e) {
      setError(errorText(e));
    }
  };

  const revoke = async (device: DeviceInfo) => {
    if (!window.confirm(`Отозвать устройство «${device.name}»?`)) return;
    setRevokingId(device.id);
    setError("");
    try {
      await api("DELETE", `/api/devices/${encodeURIComponent(device.id)}`);
      await refreshDevices();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setRevokingId(null);
    }
  };

  return (
    <section className="settings-section-card">
      <header>
        <h1>Устройства</h1>
        <p>Подключение мобильного приложения Lognara по QR-коду.</p>
      </header>
      <div className="settings-section-body device-settings">
        <section className="device-pairing" aria-labelledby="device-pairing-title">
          <h2 id="device-pairing-title">Подключить мобильное приложение</h2>
          <p className="muted">Откройте приложение Lognara и отсканируйте код.</p>

          {pairing && (
            <div className="device-pairing-active">
              <div className="device-pairing-code">
                <div className="device-qr-frame"><DeviceQr payload={pairing.payload} /></div>
                <p className="device-countdown" role="status">Код действует ещё <strong>{formatCountdown(secondsLeft)}</strong></p>
                <button
                  type="button"
                  className="quiet"
                  aria-expanded={manualCodeVisible}
                  onClick={() => setManualCodeVisible((visible) => !visible)}
                >
                  {manualCodeVisible ? "Скрыть код для ручного ввода" : "Показать код для ручного ввода"}
                </button>
                {manualCodeVisible && (
                  <div className="device-manual-code">
                    <label>
                      Код для ручного ввода
                      <textarea readOnly value={pairing.payload} />
                    </label>
                    <button type="button" className="quiet" onClick={copyManualCode}>{copied ? "Скопировано" : "Скопировать код"}</button>
                  </div>
                )}
              </div>
              <div className="device-pairing-actions">
                <button type="button" className="solid" onClick={createPairing} disabled={creating}>
                  {creating ? "Создаём код…" : "Создать новый код"}
                </button>
              </div>
            </div>
          )}

          {!pairing && pairingResult === "idle" && (
            <button type="button" className="solid" onClick={createPairing} disabled={creating}>
              {creating ? "Создаём код…" : "Подключить устройство"}
            </button>
          )}
          {!pairing && pairingResult === "used" && <p className="ok" role="status">Устройство подключено</p>}
          {!pairing && pairingResult === "expired" && <p className="device-pairing-result" role="status">Код истёк</p>}
          {!pairing && pairingResult === "replaced" && <p className="device-pairing-result" role="status">Код заменён в другой вкладке или окне</p>}
          {!pairing && pairingResult !== "idle" && (
            <button type="button" className="solid" onClick={createPairing} disabled={creating}>
              {creating ? "Создаём код…" : "Создать новый код"}
            </button>
          )}
          {error && <p className="error" role="alert">{error}</p>}
        </section>

        <section className="device-list-section" aria-labelledby="device-list-title">
          <h2 id="device-list-title">Подключённые устройства</h2>
          {listError && <p className="error" role="alert">{listError}</p>}
          {listLoading ? (
            <p className="muted" role="status">Загружаем устройства…</p>
          ) : (
            <ul className="devices-list">
              {devices.map((device) => (
                <li className="device-record" key={device.id}>
                  <div className="device-record-info">
                    <strong>{device.name}</strong>
                    <span className="muted">Подключено {formatTime(device.created_at)}</span>
                    <span className="muted">Последнее использование {formatTime(device.last_used_at)}</span>
                  </div>
                  <button type="button" className="quiet" onClick={() => void revoke(device)} disabled={revokingId !== null}>
                    {revokingId === device.id ? "Отзываем…" : "Отозвать"}
                  </button>
                </li>
              ))}
              {devices.length === 0 && <li className="muted">Устройств нет.</li>}
            </ul>
          )}
        </section>
      </div>
    </section>
  );
}
