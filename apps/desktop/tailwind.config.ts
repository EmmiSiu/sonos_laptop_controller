import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{vue,ts}"],
  theme: {
    extend: {
      colors: {
        // A dark-first palette with one accent. Deliberately small: a controller that lives
        // in the corner of a screen should be legible, not decorative.
        ink: {
          950: "#07090d",
          900: "#0d1117",
          800: "#161b22",
          700: "#21262d",
          600: "#30363d",
        },
        accent: {
          DEFAULT: "#6ee7b7",
          dim: "#34d399",
        },
      },
      fontFamily: {
        sans: ["Inter", "Segoe UI Variable", "Segoe UI", "system-ui", "sans-serif"],
        mono: ["Cascadia Code", "Consolas", "ui-monospace", "monospace"],
      },
      animation: {
        "radar": "radar 2.4s cubic-bezier(0.2, 0.6, 0.3, 1) infinite",
        "breathe": "breathe 3s ease-in-out infinite",
      },
      keyframes: {
        radar: {
          "0%": { transform: "scale(0.35)", opacity: "0.55" },
          "100%": { transform: "scale(1)", opacity: "0" },
        },
        breathe: {
          "0%, 100%": { opacity: "0.65" },
          "50%": { opacity: "1" },
        },
      },
    },
  },
  plugins: [],
} satisfies Config;
