# ⚡ Spark Bridge

Aplicativo leve para PC feito com **Tauri v2** (Rust + WebView) que cria uma ponte segura entre o seu computador e agentes de IA (como **Gemini Spark** e clientes **MCP**), gerando um **link público grátis** via Cloudflare Quick Tunnel.

## ✨ Funcionalidades

- **Link Público Grátis:** Gera automaticamente uma URL `https://....trycloudflare.com` (baixa o `cloudflared` sozinho se não estiver instalado).
- **MCP Streamable HTTP completo:** Endpoint `POST /mcp` (JSON-RPC 2.0) + `GET /mcp` (stream SSE), com negociação de versão do protocolo (`2025-03-26`, `2025-06-18`, `2024-11-05`), `Mcp-Session-Id`, notificações `202` e CORS com headers MCP expostos.
- **Autenticação flexível:** token via `?token=...` na URL, header `Authorization: Bearer ...` ou `X-Api-Key`.
- **Executar em Segundo Plano:** Minimiza para a bandeja do sistema (*System Tray*) ao fechar a janela.
- **Iniciar com o Sistema:** Opção para abrir automaticamente quando o PC ligar.
- **Teste de conexão integrado:** botão **Testar conexão MCP** faz o handshake (`initialize` + `tools/list`) e mostra latência e quantidade de ferramentas.
- **Configuração pronta por cliente:** gera o JSON para Claude Desktop, Cursor, VS Code e Gemini CLI com um clique.
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
2. Copie o campo **URL Completa para MCP / Agente (Link + Token)** (`https://xxxx.trycloudflare.com/mcp?token=sb_...`).
3. Na seção **Configuração do cliente MCP**, escolha seu cliente e copie o JSON gerado.
4. Cole a configuração no cliente MCP e reconecte.

> ⚠️ URLs do Cloudflare Quick Tunnel mudam a cada início. Sempre copie a URL nova após reconectar.

## 🔌 Conectar clientes MCP

### Claude Desktop (`claude_desktop_config.json`)

```json
{
  "mcpServers": {
    "spark-bridge": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://SEU-TUNEL.trycloudflare.com/mcp", "--header", "Authorization: Bearer sb_SEU_TOKEN"]
    }
  }
}
```

### Cursor (`.cursor/mcp.json`)

```json
{
  "mcpServers": {
    "spark-bridge": {
      "url": "https://SEU-TUNEL.trycloudflare.com/mcp",
      "headers": { "Authorization": "Bearer sb_SEU_TOKEN" }
    }
  }
}
```

### VS Code (`.vscode/mcp.json`)

```json
{
  "servers": {
    "spark-bridge": {
      "type": "http",
      "url": "https://SEU-TUNEL.trycloudflare.com/mcp",
      "headers": { "Authorization": "Bearer sb_SEU_TOKEN" }
    }
  }
}
```

### Gemini CLI (`~/.gemini/settings.json`)

```json
{
  "mcpServers": {
    "spark-bridge": {
      "httpUrl": "https://SEU-TUNEL.trycloudflare.com/mcp",
      "headers": { "Authorization": "Bearer sb_SEU_TOKEN" }
    }
  }
}
```

## 🛠️ Troubleshooting

### Erro: “Não foi possível acessar o servidor MCP. Revise o URL”

Esse erro significa que o cliente não conseguiu completar o handshake MCP. Verifique, nesta ordem:

1. **Use a URL com `/mcp`:** `https://....trycloudflare.com/mcp?token=sb_...` (não apenas `/`).
2. **Token atualizado:** a URL e o token mudam a cada início do app — copie novamente após reconectar.
3. **Status Online:** o Spark Bridge precisa estar com status **Online** e o túnel ativo.
4. **Teste integrado:** clique em **Testar conexão MCP** no app. Se falhar, o problema está no servidor local/túnel, não no cliente.
5. **Header vs. query:** se o cliente suporta headers, use a URL base (`.../mcp`) + `Authorization: Bearer <token>`. Se não suporta, use a URL completa com `?token=...`.
6. **Health check:** abra `https://....trycloudflare.com/health` no navegador — deve retornar `{"status":"ok",...}`.

## 🧪 Desenvolvimento

```bash
npm install
npx tauri dev
```

## 📦 Release

As releases são criadas pelo workflow **Build & Release Spark Bridge** (`.github/workflows/release.yml`) a partir de tags `v*`:

```bash
# 1. Atualize a versão em package.json, src-tauri/Cargo.toml e src-tauri/tauri.conf.json
# 2. Commit e push na main
git push origin main
# 3. Crie e envie a tag
git tag v0.2.0
git push origin v0.2.0
```

O workflow compila para Linux, Windows e macOS e publica os instaladores na aba Releases. Também é possível rodar manualmente em **Actions → Build & Release Spark Bridge → Run workflow** informando a tag.
