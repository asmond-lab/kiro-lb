import { useSyncExternalStore } from "react";

export const THEMES = ["dark", "light"] as const;
export type Theme = (typeof THEMES)[number];

export const LANGUAGES = [
  { id: "en-US", label: "English (US)" },
  { id: "ko-KR", label: "한국어" },
  { id: "zh-CN", label: "中文" },
  { id: "pt-BR", label: "Português" },
] as const;
export type Language = (typeof LANGUAGES)[number]["id"];

type Preferences = { theme: Theme; language: Language };

const STORAGE_KEY = "kiro-lb-preferences";
const DEFAULTS: Preferences = { theme: "dark", language: "en-US" };

const MESSAGES = {
  "en-US": {
    overview: "Overview",
    accounts: "Accounts",
    apiKeys: "API keys",
    navigation: "Navigation",
    navMonitoring: "Monitoring",
    navManagement: "Management",
    navSystem: "System",
    settings: "Settings",
    theme: "Theme",
    info: "Info",
    live: "Live",
    paused: "Paused",
    refresh: "Refresh",
    refreshUsage: "Refresh usage",
    signOut: "Sign out",
    signIn: "Sign in",
    dashboardPassword: "Dashboard password",
    loadingDashboard: "Loading dashboard…",
    appearance: "Appearance",
    appearanceDescription: "Theme and language for this dashboard.",
    themeLabel: "Theme",
    dark: "Dark",
    light: "Light",
    languageLabel: "Language",
  },
  "ko-KR": {
    overview: "개요",
    accounts: "계정",
    apiKeys: "API 키",
    navigation: "탐색",
    navMonitoring: "모니터링",
    navManagement: "관리",
    navSystem: "시스템",
    settings: "설정",
    theme: "테마",
    info: "정보",
    live: "실시간",
    paused: "일시정지",
    refresh: "새로고침",
    refreshUsage: "사용량 새로고침",
    signOut: "로그아웃",
    signIn: "로그인",
    dashboardPassword: "대시보드 비밀번호",
    loadingDashboard: "대시보드 로딩 중…",
    appearance: "화면",
    appearanceDescription: "이 대시보드의 테마와 언어입니다.",
    themeLabel: "테마",
    dark: "다크",
    light: "라이트",
    languageLabel: "언어",
  },
  "zh-CN": {
    overview: "概览",
    accounts: "账户",
    apiKeys: "API 密钥",
    navigation: "导航",
    navMonitoring: "监控",
    navManagement: "管理",
    navSystem: "系统",
    settings: "设置",
    theme: "主题",
    info: "信息",
    live: "实时",
    paused: "已暂停",
    refresh: "刷新",
    refreshUsage: "刷新用量",
    signOut: "退出登录",
    signIn: "登录",
    dashboardPassword: "仪表板密码",
    loadingDashboard: "正在加载仪表板…",
    appearance: "外观",
    appearanceDescription: "此仪表板的主题和语言。",
    themeLabel: "主题",
    dark: "深色",
    light: "浅色",
    languageLabel: "语言",
  },
  "pt-BR": {
    overview: "Visão geral",
    accounts: "Contas",
    apiKeys: "Chaves de API",
    navigation: "Navegação",
    navMonitoring: "Monitoramento",
    navManagement: "Gerenciamento",
    navSystem: "Sistema",
    settings: "Configurações",
    theme: "Tema",
    info: "Info",
    live: "Ao vivo",
    paused: "Pausado",
    refresh: "Atualizar",
    refreshUsage: "Atualizar uso",
    signOut: "Sair",
    signIn: "Entrar",
    dashboardPassword: "Senha do dashboard",
    loadingDashboard: "Carregando dashboard…",
    appearance: "Aparência",
    appearanceDescription: "Tema e idioma deste dashboard.",
    themeLabel: "Tema",
    dark: "Escuro",
    light: "Claro",
    languageLabel: "Idioma",
  },
} satisfies Record<Language, Record<string, string>>;

export type MessageKey = keyof (typeof MESSAGES)["en-US"];

function read(): Preferences {
  try {
    const raw = JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}") as Partial<Preferences>;
    return {
      theme: THEMES.includes(raw.theme as Theme) ? (raw.theme as Theme) : DEFAULTS.theme,
      language: LANGUAGES.some((l) => l.id === raw.language) ? (raw.language as Language) : DEFAULTS.language,
    };
  } catch {
    return DEFAULTS;
  }
}

let current: Preferences = typeof window === "undefined" ? DEFAULTS : read();
const listeners = new Set<() => void>();

export function applyTheme(theme: Theme = current.theme) {
  document.documentElement.classList.toggle("dark", theme === "dark");
  document.documentElement.style.colorScheme = theme;
}

export function applyLanguage(language: Language = current.language) {
  document.documentElement.lang = language;
}

function persist(): boolean {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(current));
    return true;
  } catch {
    return false;
  }
}

export function setPreferences(patch: Partial<Preferences>) {
  current = { ...current, ...patch };
  persist();
  applyTheme();
  applyLanguage();
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

type Catalog = Record<Language, Record<string, string>>;
const EXTRA = import.meta.glob<Catalog>("./i18n/*.ts", { eager: true, import: "default" });

export function translate(language: Language, key: MessageKey | (string & {}), vars?: Record<string, string | number>): string {
  let text: string | undefined = (MESSAGES[language] as Record<string, string>)[key];
  if (text === undefined) {
    for (const catalog of Object.values(EXTRA)) {
      text = catalog[language]?.[key] ?? text;
    }
  }
  if (text === undefined) {
    text = (MESSAGES["en-US"] as Record<string, string>)[key];
    for (const catalog of Object.values(EXTRA)) text ??= catalog["en-US"]?.[key];
  }
  text ??= key;
  return vars ? text.replace(/\{(\w+)\}/g, (_, name: string) => String(vars[name] ?? `{${name}}`)) : text;
}

export function usePreferences() {
  const prefs = useSyncExternalStore(subscribe, () => current, () => DEFAULTS);
  return {
    ...prefs,
    t: (key: MessageKey | (string & {}), vars?: Record<string, string | number>) => translate(prefs.language, key, vars),
  };
}
