import { ArrowLeft, ExternalLink, Sparkles, UsersRound } from "lucide-react";

interface AboutViewProps {
  onBack: () => void;
}

const DEVELOPERS = ["GPT-6 luna", "Qwen3.8 max", "deepseek-v4.1", "D1zholl"];

export function AboutView({ onBack }: AboutViewProps) {
  return (
    <>
      <section className="page-heading about-page-heading">
        <div>
          <div className="eyebrow"><Sparkles size={15} /> ЗА КАДРОМ</div>
          <h1>Разработчики</h1>
          <p className="page-subtitle">Люди и нейросети, благодаря которым появился MyLauncher.</p>
        </div>
        <button className="button-secondary about-back" type="button" onClick={onBack}>
          <ArrowLeft size={15} /> Назад
        </button>
      </section>

      <section className="about-developers" aria-label="Разработчики проекта">
        {DEVELOPERS.map((developer) => (
          <article className="about-developer" key={developer}>
            <span className="about-developer-icon"><UsersRound size={18} /></span>
            <strong>{developer}</strong>
          </article>
        ))}
      </section>

      <section className="about-project-note">
        <h2>Как создавался проект</h2>
        <p>
          Проект был создан тремя нейросетями: GPT-6 писал код в VS Code, Qwen3.8 max помогал с идеями,
          реализацией, настройкой API и работой некоторых функций, deepseek v4.1 анализировал и исправлял
          некоторые ошибки.
        </p>
        <p className="about-budget">Бюджет: 5 аккаунтов гугла, 3 бутылки palpy, пачка doritos, розетка на 220v.</p>
      </section>

      <a
        className="about-social-link"
        href="https://www.tiktok.com/@d1zholl?_r=1&_t=ZS-9AwMKcfDFQa"
        target="_blank"
        rel="noreferrer"
      >
        <ExternalLink size={15} /> TikTok @d1zholl
      </a>
    </>
  );
}