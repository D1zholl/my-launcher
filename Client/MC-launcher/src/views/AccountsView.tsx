import { CheckCircle2, CircleUserRound, LoaderCircle, Plus, Trash2 } from "lucide-react";
import type { Account, DeviceCodeInfo } from "../types";

interface AccountsViewProps {
  accounts: Account[];
  activeAccountId: string;
  microsoftClientId: string;
  offlineUsername: string;
  deviceCode: DeviceCodeInfo | null;
  microsoftBusy: boolean;
  onMicrosoftClientIdChange: (value: string) => void;
  onSaveMicrosoftClientId: (value: string) => void;
  onBeginMicrosoftLogin: () => void;
  onCompleteMicrosoftLogin: () => void;
  onOpenVerification: () => void;
  onOfflineUsernameChange: (value: string) => void;
  onAddOfflineAccount: (event: React.FormEvent<HTMLFormElement>) => void;
  onSelectAccount: (account: Account) => void;
  onDeleteAccount: (account: Account) => void;
}

export function AccountsView({
  accounts,
  activeAccountId,
  microsoftClientId,
  offlineUsername,
  deviceCode,
  microsoftBusy,
  onMicrosoftClientIdChange,
  onSaveMicrosoftClientId,
  onBeginMicrosoftLogin,
  onCompleteMicrosoftLogin,
  onOpenVerification,
  onOfflineUsernameChange,
  onAddOfflineAccount,
  onSelectAccount,
  onDeleteAccount,
}: AccountsViewProps) {
  return (
    <>
      <section className="page-heading"><div><div className="eyebrow"><CircleUserRound size={15} /> ПРОФИЛИ</div><h1>Аккаунты</h1><p className="page-subtitle">Локальное хранилище аккаунтов.</p></div></section>
      <section className="settings-section auth-section">
        <div className="settings-section-heading"><span>MICROSOFT</span><p>Официальный аккаунт с лицензией Minecraft Java Edition.</p></div>
        <label className="form-field"><span>Публичный Client ID приложения</span><input value={microsoftClientId} onChange={(event) => onMicrosoftClientIdChange(event.target.value)} onBlur={(event) => onSaveMicrosoftClientId(event.currentTarget.value)} placeholder="Client ID из Microsoft Entra" /></label>
        {deviceCode ? (
          <div className="device-code-panel">
            <strong>{deviceCode.userCode}</strong>
            <span>Введите код на странице авторизации Microsoft.</span>
            <button className="button-secondary" onClick={onOpenVerification}>Открыть страницу входа</button>
            <button className="button-primary" onClick={onCompleteMicrosoftLogin} disabled={microsoftBusy}>{microsoftBusy ? <LoaderCircle className="spin" size={16} /> : <CheckCircle2 size={16} />} Я подтвердил вход</button>
          </div>
        ) : (
          <button className="button-primary auth-button" onClick={onBeginMicrosoftLogin} disabled={microsoftBusy || !microsoftClientId.trim()}>{microsoftBusy ? <LoaderCircle className="spin" size={16} /> : <CircleUserRound size={16} />} Войти через Microsoft</button>
        )}
      </section>
      <form className="account-create-form" onSubmit={onAddOfflineAccount}>
        <label className="form-field"><span>Никнейм offline-аккаунта</span><input value={offlineUsername} onChange={(event) => onOfflineUsernameChange(event.target.value)} maxLength={16} pattern="[A-Za-z0-9_]{1,16}" placeholder="Player123" required /></label>
        <button className="button-primary" type="submit"><Plus size={17} /> Добавить аккаунт</button>
      </form>
      {accounts.length === 0 ? (
        <section className="section-empty"><div className="section-empty-icon"><CircleUserRound size={24} /></div><h2>Аккаунтов пока нет</h2><p>Добавьте offline-профиль или войдите через Microsoft.</p></section>
      ) : (
        <section className="account-list" aria-label="Локальные аккаунты">
          {accounts.map((account) => (
            <article className="account-row" key={account.id}>
              <CircleUserRound size={20} />
              <div className="account-row-copy"><strong>{account.username}</strong><span>{account.account_type === "offline" ? `Offline · UUID ${account.uuid.slice(0, 8)}…` : "Microsoft · лицензия подтверждена"}</span></div>
              <button className={`button-secondary ${activeAccountId === account.id ? "account-selected" : ""}`} onClick={() => onSelectAccount(account)}>{activeAccountId === account.id ? "Активен" : "Выбрать"}</button>
              <button className="icon-button danger-button" aria-label={`Удалить аккаунт ${account.username}`} title="Удалить аккаунт" onClick={() => onDeleteAccount(account)}><Trash2 size={16} /></button>
            </article>
          ))}
        </section>
      )}
    </>
  );
}
