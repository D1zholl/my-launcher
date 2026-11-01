import { useState } from "react";
import { Boxes, Check, ChevronRight, Download, LoaderCircle, RefreshCw, Server } from "lucide-react";
import { listRepositoryPacks, listRepositoryVersions } from "../lib/repository";
import type { RepositoryPack, RepositoryPackVersion } from "../types";

interface RepositoryViewProps {
  reportError: (error: unknown) => void;
  onInstall: (input: { name: string; repository: string; pack: RepositoryPack; version: RepositoryPackVersion }) => Promise<void>;
}

function safeInstanceName(name: string) {
  return name.replace(/[\\/:*?"<>|]/g, "-").replace(/[. ]+$/g, "").slice(0, 64).trim() || "Сборка";
}

function safeIconUrl(iconUrl: string | null, repository: string): string | null {
  if (!iconUrl) return null;
  try {
    const url = new URL(iconUrl, repository);
    return url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

export function RepositoryView({ reportError, onInstall }: RepositoryViewProps) {
  const [repository, setRepository] = useState(() => localStorage.getItem("mylauncher-repository-url") ?? "");
  const [packs, setPacks] = useState<RepositoryPack[]>([]);
  const [selectedPack, setSelectedPack] = useState<RepositoryPack | null>(null);
  const [versions, setVersions] = useState<RepositoryPackVersion[]>([]);
  const [channel, setChannel] = useState<"stable" | "beta">(() => localStorage.getItem("mylauncher-repository-channel") === "beta" ? "beta" : "stable");
  const [selectedVersion, setSelectedVersion] = useState("");
  const [instanceName, setInstanceName] = useState("");
  const [busy, setBusy] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [trustedSource, setTrustedSource] = useState(false);

  async function loadPacks(event?: React.FormEvent<HTMLFormElement>) {
    event?.preventDefault();
    const url = repository.trim().replace(/\/+$/, "");
    if (!url) return;
    setBusy(true);
    setSelectedPack(null);
    try {
      const results = await listRepositoryPacks(url, channel);
      setPacks(results);
      localStorage.setItem("mylauncher-repository-url", url);
      setRepository(url);
    } catch (error) {
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function selectPack(pack: RepositoryPack, selectedChannel = channel) {
    setBusy(true);
    setSelectedPack(pack);
    setVersions([]);
    setSelectedVersion("");
    setTrustedSource(false);
    setInstanceName(safeInstanceName(pack.name));
    try {
      const result = await listRepositoryVersions(repository, pack.id, selectedChannel);
      setVersions(result);
      setSelectedVersion(result[0]?.version ?? "");
    } catch (error) {
      setSelectedPack(null);
      reportError(error);
    } finally {
      setBusy(false);
    }
  }

  async function changeChannel(nextChannel: "stable" | "beta") {
    setChannel(nextChannel);
    localStorage.setItem("mylauncher-repository-channel", nextChannel);
    const url = repository.trim().replace(/\/+$/, "");
    if (url) {
      setBusy(true);
      try {
        const results = await listRepositoryPacks(url, nextChannel);
        setPacks(results);
        const refreshedPack = results.find((pack) => pack.id === selectedPack?.id);
        if (refreshedPack) await selectPack(refreshedPack, nextChannel);
        else { setSelectedPack(null); setVersions([]); }
      } catch (error) {
        reportError(error);
      } finally {
        setBusy(false);
      }
    }
  }

  async function submitInstall(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const version = versions.find((item) => item.version === selectedVersion);
    if (!selectedPack || !version || !instanceName.trim() || installing) return;
    setInstalling(true);
    try {
      await onInstall({ name: instanceName.trim(), repository, pack: selectedPack, version });
      setSelectedPack(null);
    } catch (error) {
      reportError(error);
    } finally {
      setInstalling(false);
    }
  }

  return (
    <>
      <section className="page-heading">
        <div><div className="eyebrow"><Boxes size={15} /> MYLAUNCHER REPOSITORY</div><h1>Собственные сборки</h1><p className="page-subtitle">Репозиторий может устанавливать файлы и задавать параметры запуска. Подключайте только доверенные источники.</p></div>
      </section>
      <form className="modpack-toolbar" onSubmit={(event) => void loadPacks(event)}>
        <label className="mod-search-field"><Server size={17} /><input type="url" value={repository} onChange={(event) => setRepository(event.target.value)} placeholder="https://packs.example.com или http://192.168.1.10:8787" aria-label="Адрес репозитория" required /></label>
        <label className="form-field repository-channel"><span>Канал</span><select value={channel} onChange={(event) => void changeChannel(event.target.value as "stable" | "beta")}><option value="stable">Стабильный</option><option value="beta">Бета</option></select></label>
        <button className="button-primary" type="submit" disabled={busy}>{busy ? <LoaderCircle className="spin" size={16} /> : <RefreshCw size={15} />} Подключить</button>
      </form>
      {busy && packs.length === 0 ? <div className="mod-catalog-loading"><LoaderCircle className="spin" size={20} /> Подключаемся к репозиторию...</div> : packs.length ? (
        <section className="modpack-results" aria-label="Сборки собственного репозитория">
          {packs.map((pack) => (
            <article className="mod-catalog-card modpack-card" key={pack.id}>
              {safeIconUrl(pack.iconUrl, repository) ? <img className="mod-project-icon" src={safeIconUrl(pack.iconUrl, repository)!} alt="" /> : <div className="mod-file-icon"><Boxes size={18} /></div>}
              <div className="mod-copy"><div className="mod-card-title"><strong>{pack.name}</strong><span>от {pack.authors.join(", ") || "автора"}</span></div><span className="mod-card-description">{pack.description}</span><div className="mod-tags">{pack.categories.slice(0, 4).map((tag) => <span className="mod-tag" key={tag}>{tag}</span>)}<span className="mod-tag">{pack.latestVersion}</span>{channel === "beta" && <span className="mod-tag">БЕТА</span>}</div></div>
              <div className="mod-card-actions"><button className="button-secondary" type="button" onClick={() => void selectPack(pack)} disabled={busy}><Download size={15} /> Версии <ChevronRight size={14} /></button></div>
            </article>
          ))}
        </section>
      ) : <section className="section-empty mod-catalog-empty"><div className="section-empty-icon"><Server size={24} /></div><h2>Подключите репозиторий</h2><p>Введите HTTPS-адрес или адрес сервера в локальной сети, чтобы посмотреть опубликованные сборки.</p></section>}
      {selectedPack && (
        <section className="instance-dialog modpack-dialog repository-install" role="region" aria-labelledby="repository-install-title">
          <div className="dialog-heading"><div><span className="eyebrow">СОБСТВЕННЫЙ РЕПОЗИТОРИЙ</span><h2 id="repository-install-title">{selectedPack.name}</h2></div><button className="dialog-close" type="button" onClick={() => setSelectedPack(null)} disabled={installing}>×</button></div>
          <form onSubmit={(event) => void submitInstall(event)}>
            <label className="form-field"><span>Версия сборки</span><select value={selectedVersion} onChange={(event) => setSelectedVersion(event.target.value)} required><option value="" disabled>Выберите версию</option>{versions.map((version) => <option key={version.version} value={version.version}>{version.version} · {version.channel === "beta" ? "Бета" : "Стабильная"}</option>)}</select></label>
            <label className="form-field"><span>Имя инстанса</span><input value={instanceName} onChange={(event) => setInstanceName(event.target.value)} maxLength={64} required pattern={'[^\\/:*?"<>|]+'} /></label>
            <label className="repository-trust"><input type="checkbox" checked={trustedSource} onChange={(event) => setTrustedSource(event.target.checked)} required /><span>Я доверяю этому источнику и разрешаю устанавливать его файлы и параметры запуска.</span></label>
            <div className="dialog-actions"><button className="button-secondary" type="button" onClick={() => setSelectedPack(null)} disabled={installing}>Отмена</button><button className="button-primary" type="submit" disabled={busy || installing || !selectedVersion || !instanceName.trim()}>{installing ? <LoaderCircle className="spin" size={16} /> : <Check size={16} />} Установить</button></div>
          </form>
        </section>
      )}
    </>
  );
}