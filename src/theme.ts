export type Theme = "light" | "dark";

const KEY = "llp-theme";

function stored(): Theme | null {
  try {
    const value = localStorage.getItem(KEY);
    return value === "light" || value === "dark" ? value : null;
  } catch {
    return null;
  }
}

function persist(theme: Theme): void {
  try {
    localStorage.setItem(KEY, theme);
  } catch {
    document.documentElement.dataset.theme = theme;
  }
}

export function initThemeToggle(
  button: HTMLButtonElement,
  onExplicitChange: (explicit: Theme | null) => void = () => {},
): void {
  const light = window.matchMedia("(prefers-color-scheme: light)");

  const current = (): Theme => stored() ?? (light.matches ? "light" : "dark");
  const sync = (): void => {
    const theme = current();
    document.documentElement.dataset.theme = theme;
    button.title = theme === "dark" ? "切换到浅色主题" : "切换到深色主题";
    onExplicitChange(stored());
  };

  sync();
  button.addEventListener("click", () => {
    persist(current() === "dark" ? "light" : "dark");
    sync();
  });
  light.addEventListener("change", () => {
    if (!stored()) sync();
  });
}
