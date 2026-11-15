# ⚡ Spark Bridge

Aplicativo leve para PC feito com **Tauri v2** (Rust + WebView) que cria uma ponte segura entre o seu computador e agentes de IA (como **Gemini Spark** e clientes **MCP**), gerando um **link público grátis** via Cloudflare Quick Tunnel.

## ✨ Funcionalidades

- **Link Público Grátis:** Gera automaticamente uma URL `https://....trycloudflare.com` (baixa o `cloudflared` sozinho se não estiver instalado).
- **Compatível com MCP (Model Context Protocol):** Suporta JSON-RPC 2.0 nas rotas `/` e `/mcp` com autenticação via `?token=...` ou `Authorization: Bearer ...`.
- **Executar em Segundo Plano:** Minimiza para a bandeja do sistema (*System Tray*) ao fechar a janela.
- **Iniciar com o Sistema:** Opção para abrir automaticamente quando o PC ligar.
- **Controle Completo para Agentes:**
  - `screen_capture` / `GET /screen` — Captura a tela em PNG (base64).
  - `mouse_move`, `mouse_click`, `mouse_scroll` — Move, clica e rola o mouse.
  - `keyboard_type`, `keyboard_key` — Digita textos e pressiona teclas/atalhos.
  - `terminal_exec` / `POST /terminal/exec` — Executa comandos no terminal.

## 📥 Download

Baixe o instalador para o seu sistema na aba [**Releases**](https://github.com/Pedro21062014/spark-bridge/releases):

- **Linux:** `.deb` ou `.AppImage`
- **Windows:** `.exe` (ou `.msi`)
- **macOS:** `.dmg`

## 🚀 Como usar

1. Abra o **Spark Bridge** e clique em **Iniciar Conexão**.
2. Copie o campo **URL Completa para MCP / Agente (Link + Token)** (`https://xxxx.trycloudflare.com/?token=sb_...`).
3. Cole essa URL diretamente no seu agente de IA / cliente MCP para conectar.

## 🛠️ Desenvolvimento

```bash
npm install
npx tauri dev
```
