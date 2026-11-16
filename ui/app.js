const hasTauri = !!(window.__TAURI__ && window.__TAURI__.core);
const invoke = hasTauri ? window.__TAURI__.core.invoke : null;

// ---------- Tema claro/escuro ----------
const themeToggle = document.getElementById("theme-toggle");

function applyTheme(theme) {
  document.documentElement.setAttribute("data-theme", theme);
  themeToggle.textContent = theme === "light" ? "🌙" : "☀️";
  try {
    localStorage.setItem("spark-theme", theme);
  } catch {
    // localStorage indisponivel (ex.: preview) — ignora
  }
}

function loadTheme() {
  let theme = "dark";
  try {
    theme = localStorage.getItem("spark-theme") || "dark";
  } catch {
    theme = "dark";
  }
  applyTheme(theme);
}

themeToggle.addEventListener("click", () => {
  const current = document.documentElement.getAttribute("data-theme") || "dark";
  applyTheme(current === "light" ? "dark" : "light");
});

loadTheme();

const toggleBtn = document.getElementById("toggle-btn");
const btnText = document.getElementById("btn-text");
const statusBadge = document.getElementById("status-badge");
const statusText = document.getElementById("status-text");
const mcpUrlInput = document.getElementById("mcp-url");
const authHeaderInput = document.getElementById("auth-header");
const publicUrlInput = document.getElementById("public-url");
const localUrlInput = document.getElementById("local-url");
const apiTokenInput = document.getElementById("api-token");
const tokenEyeBtn = document.getElementById("token-eye");
const regenerateBtn = document.getElementById("regenerate-btn");
const testBtn = document.getElementById("test-btn");
const testResult = document.getElementById("test-result");
const clientSelect = document.getElementById("client-select");
const configOutput = document.getElementById("config-output");
const configHint = document.getElementById("config-hint");
const copyConfigBtn = document.getElementById("copy-config-btn");
const providerSelect = document.getElementById("provider-select");
const providerHint = document.getElementById("provider-hint");
const ngrokFields = document.getElementById("ngrok-fields");
const ngrokToken = document.getElementById("ngrok-token");
const ngrokDomain = document.getElementById("ngrok-domain");
const ngrokEye = document.getElementById("ngrok-eye");
const saveTunnelBtn = document.getElementById("save-tunnel-btn");
const bgToggle = document.getElementById("bg-toggle");
const autostartToggle = document.getElementById("autostart-toggle");
const errorMsg = document.getElementById("error-msg");
const toastEl = document.getElementById("toast");
const tauriMissing = document.getElementById("tauri-missing");
const connectionTab = document.getElementById("connection-tab");
const securityTab = document.getElementById("security-tab");
const securityPanel = document.getElementById("security-panel");
const permissionsList = document.getElementById("permissions-list");
const savePermissionsBtn = document.getElementById("save-permissions-btn");
const enableAllBtn = document.getElementById("enable-all-btn");
const disableAllBtn = document.getElementById("disable-all-btn");
const checkComponentsBtn = document.getElementById("check-components-btn");
const componentsResult = document.getElementById("components-result");

let isRunning = false;
let currentStatus = null;
let toastTimer = null;

const TOOL_LABELS = {
  screen_capture: "Capturar tela",
  mouse_move: "Mover mouse",
  mouse_click: "Clicar",
  mouse_scroll: "Rolar",
  keyboard_type: "Digitar texto",
  keyboard_key: "Pressionar teclas e atalhos",
  terminal_exec: "Executar comandos no terminal",
  mouse_drag: "Segurar e arrastar mouse",
  get_screen_info: "Ler informações dos monitores",
  clipboard_read: "Ler área de transferência",
  clipboard_write: "Escrever na área de transferência",
  open_url: "Abrir URLs",
  wait: "Aguardar",
  screen_ascii: "Ler tela em ASCII",
  ocr_screen: "Executar OCR da tela",
};

function renderPermissions(permissions) {
  permissionsList.replaceChildren();
  Object.entries(TOOL_LABELS).forEach(([name, label]) => {
    const row = document.createElement("label");
    row.className = "permission-row";
    const text = document.createElement("span");
    text.innerHTML = `<strong>${label}</strong><small>${name}</small>`;
    const checkbox = document.createElement("input");
    checkbox.type = "checkbox";
    checkbox.dataset.permission = name;
    checkbox.checked = permissions[name] !== false;
    row.append(text, checkbox);
    permissionsList.append(row);
  });
}

function setSecurityTab(active) {
  connectionTab.classList.toggle("active", !active);
  securityTab.classList.toggle("active", active);
  connectionTab.setAttribute("aria-selected", String(!active));
  securityTab.setAttribute("aria-selected", String(active));
  securityPanel.classList.toggle("hidden", !active);
  document.querySelectorAll(".connection-view").forEach((element) => element.classList.toggle("hidden", active));
}

async function loadPermissions() {
  if (!hasTauri) {
    renderPermissions(Object.fromEntries(Object.keys(TOOL_LABELS).map((name) => [name, true])));
    return;
  }
  try {
    renderPermissions(await invoke("get_permissions"));
  } catch (err) {
    permissionsList.textContent = `Não foi possível carregar as permissões: ${err}`;
  }
}

async function savePermissions() {
  if (!hasTauri) return;
  const permissions = Object.fromEntries(
    [...permissionsList.querySelectorAll("input[data-permission]")].map((input) => [input.dataset.permission, input.checked])
  );
  savePermissionsBtn.disabled = true;
  try {
    renderPermissions(await invoke("set_permissions", { permissions }));
    toast("Permissões salvas no servidor MCP.");
  } catch (err) {
    toast(`Erro ao salvar permissões: ${err}`);
  } finally {
    savePermissionsBtn.disabled = false;
  }
}

connectionTab.addEventListener("click", () => setSecurityTab(false));
securityTab.addEventListener("click", () => { setSecurityTab(true); loadPermissions(); });
savePermissionsBtn.addEventListener("click", savePermissions);
enableAllBtn.addEventListener("click", () => permissionsList.querySelectorAll("input").forEach((input) => { input.checked = true; }));
disableAllBtn.addEventListener("click", () => permissionsList.querySelectorAll("input").forEach((input) => { input.checked = false; }));

function renderComponents(components) {
  componentsResult.replaceChildren();
  for (const component of components) {
    const item = document.createElement("div");
    item.className = `component-item ${component.installed ? "ok" : "missing"}`;
    const icon = document.createElement("span");
    icon.textContent = component.installed ? "✓" : "!";
    const content = document.createElement("div");
    const title = document.createElement("strong");
    title.textContent = component.name;
    const detail = document.createElement("small");
    detail.textContent = component.detail;
    content.append(title, detail);
    if (!component.installed && component.install_hint) {
      const hint = document.createElement("code");
      hint.textContent = `Como instalar: ${component.install_hint}`;
      content.append(hint);
    }
    item.append(icon, content);
    componentsResult.append(item);
  }
}

async function checkComponents() {
  if (!hasTauri) {
    componentsResult.textContent = "Abra o Spark Bridge dentro do aplicativo Tauri para verificar o sistema.";
    return;
  }
  checkComponentsBtn.disabled = true;
  checkComponentsBtn.textContent = "Verificando...";
  try {
    renderComponents(await invoke("check_components"));
  } catch (err) {
    componentsResult.textContent = `Não foi possível verificar os componentes: ${err}`;
  } finally {
    checkComponentsBtn.disabled = false;
    checkComponentsBtn.textContent = "Verificar agora";
  }
}

checkComponentsBtn.addEventListener("click", checkComponents);

const CLIENT_HINTS = {
  claude: "Cole em claude_desktop_config.json e reinicie o Claude Desktop.",
  cursor: "Cole em .cursor/mcp.json (ou nas configurações de MCP do Cursor).",
  vscode: "Cole em .vscode/mcp.json e clique em Start no VS Code.",
  gemini: "Cole em ~/.gemini/settings.json (Gemini CLI). Usa token na URL (evita erro de OAuth).",
  generic: "Use a URL com ?token=... ou a URL base + header Authorization.",
};

function toast(msg) {
  toastEl.textContent = msg;
  toastEl.classList.remove("hidden");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.add("hidden"), 2000);
}

function mcpBaseUrl(status) {
  return status && status.public_url ? `${status.public_url}/mcp` : "";
}

function mcpFullUrl(status) {
  const base = mcpBaseUrl(status);
  return base && status.token ? `${base}?token=${status.token}` : "";
}

function buildConfig(client, status) {
  const base = mcpBaseUrl(status);
  const token = (status && status.token) || "";
  const bearer = `Bearer ${token}`;

  if (!base) {
    return "// Inicie a conexao para gerar a configuracao automaticamente.\n// A URL publica e o token aparecerao aqui.";
  }

  switch (client) {
    case "claude":
      return JSON.stringify(
        {
          mcpServers: {
            "spark-bridge": {
              command: "npx",
              args: ["-y", "mcp-remote", base, "--header", `Authorization: ${bearer}`],
            },
          },
        },
        null,
        2
      );
    case "cursor":
      return JSON.stringify(
        {
          mcpServers: {
            "spark-bridge": {
              url: base,
              headers: { Authorization: bearer },
            },
          },
        },
        null,
        2
      );
    case "vscode":
      return JSON.stringify(
        {
          servers: {
            "spark-bridge": {
              type: "http",
              url: base,
              headers: { Authorization: bearer },
            },
          },
        },
        null,
        2
      );
    case "gemini":
      // O Gemini CLI nem sempre envia headers customizados — com o token
      // na URL (?token=) ele sempre chega e evita o erro de OAuth/401.
      return JSON.stringify(
        {
          mcpServers: {
            "spark-bridge": {
              httpUrl: mcpFullUrl(status),
            },
          },
        },
        null,
        2
      );
    default:
      return [
        `URL: ${mcpFullUrl(status)}`,
        "",
        `Ou use a URL base + header:`,
        `URL: ${base}`,
        `Authorization: ${bearer}`,
      ].join("\n");
  }
}

function refreshConfig() {
  configOutput.value = buildConfig(clientSelect.value, currentStatus);
  configHint.textContent = CLIENT_HINTS[clientSelect.value] || "";
}

function refreshProviderUI() {
  const isNgrok = providerSelect.value === "ngrok";
  ngrokFields.classList.toggle("hidden", !isNgrok);
  providerHint.textContent = isNgrok
    ? "ngrok: URL fixa com seu domínio estático (requer authtoken)."
    : "Cloudflare Quick Tunnel: sem conta, URL muda a cada início.";
}

function updateUI(status) {
  currentStatus = status;
  isRunning = status.running;
  apiTokenInput.value = status.token || "";
  bgToggle.checked = status.background_mode;
  testBtn.disabled = !status.running;

  // Config do tunel (nao sobrescreve enquanto o usuario digita).
  providerSelect.value = status.tunnel_provider || "cloudflare";
  if (document.activeElement !== ngrokToken) {
    ngrokToken.value = status.ngrok_authtoken || "";
  }
  if (document.activeElement !== ngrokDomain) {
    ngrokDomain.value = status.ngrok_domain || "";
  }
  refreshProviderUI();

  if (status.running) {
    statusText.textContent = "Online";
    statusBadge.className = "badge online";
    btnText.textContent = "Parar Conexão";
    toggleBtn.classList.add("stop");
    publicUrlInput.value = status.public_url || "";
    mcpUrlInput.value = mcpFullUrl(status);
    authHeaderInput.value = status.token ? `Bearer ${status.token}` : "";
    localUrlInput.value = `${status.local_url}/mcp`;
  } else {
    statusText.textContent = "Desconectado";
    statusBadge.className = "badge offline";
    btnText.textContent = "Iniciar Conexão";
    toggleBtn.classList.remove("stop");
    publicUrlInput.value = "";
    mcpUrlInput.value = "";
    authHeaderInput.value = "";
    localUrlInput.value = "http://127.0.0.1:4891/mcp";
    testResult.classList.add("hidden");
  }

  refreshConfig();
}

async function init() {
  if (!hasTauri) {
    tauriMissing.classList.remove("hidden");
    toggleBtn.disabled = true;
    testBtn.disabled = true;
    configOutput.value =
      "// Abra este app dentro do Spark Bridge (Tauri) para gerar a configuracao.";
    return;
  }

  try {
    const status = await invoke("get_status");
    updateUI(status);
    const autostart = await invoke("get_autostart");
    autostartToggle.checked = autostart;
    await checkComponents();
    await loadPermissions();
  } catch (err) {
    console.error("Init error:", err);
  }
}

toggleBtn.addEventListener("click", async () => {
  errorMsg.classList.add("hidden");
  toggleBtn.disabled = true;

  try {
    if (!isRunning) {
      statusText.textContent = "Conectando túnel...";
      statusBadge.className = "badge loading";
      btnText.textContent = "Iniciando...";
      const status = await invoke("start_bridge");
      updateUI(status);
      toast("Conexão iniciada! Copie a URL MCP.");
    } else {
      btnText.textContent = "Parando...";
      const status = await invoke("stop_bridge");
      updateUI(status);
      toast("Conexão encerrada.");
    }
  } catch (err) {
    errorMsg.textContent = "Erro: " + err;
    errorMsg.classList.remove("hidden");
    statusText.textContent = "Desconectado";
    statusBadge.className = "badge offline";
    btnText.textContent = "Iniciar Conexão";
    toggleBtn.classList.remove("stop");
  } finally {
    toggleBtn.disabled = false;
  }
});

testBtn.addEventListener("click", async () => {
  testBtn.disabled = true;
  testBtn.textContent = "Testando...";
  testResult.className = "result-box";
  testResult.textContent = "Fazendo handshake MCP (initialize + tools/list)...";

  try {
    const result = await invoke("test_connection");
    testResult.classList.remove("hidden");
    if (result.ok) {
      testResult.classList.add("ok");
      const tools = result.tools_count != null ? ` · ${result.tools_count} ferramentas` : "";
      const version = result.server_version ? ` · servidor v${result.server_version}` : "";
      testResult.textContent = `✅ ${result.message} (${result.latency_ms} ms${tools}${version})`;
    } else {
      testResult.classList.add("err");
      testResult.textContent = `❌ ${result.message}`;
    }
  } catch (err) {
    testResult.classList.remove("hidden");
    testResult.classList.add("err");
    testResult.textContent = "❌ Erro no teste: " + err;
  } finally {
    testBtn.disabled = !isRunning;
    testBtn.textContent = "🔍 Testar conexão MCP";
  }
});

regenerateBtn.addEventListener("click", async () => {
  try {
    const status = await invoke("regenerate_token");
    updateUI(status);
    toast("Novo token gerado. Copie a nova URL MCP.");
  } catch (err) {
    errorMsg.textContent = "Erro ao gerar novo token: " + err;
    errorMsg.classList.remove("hidden");
  }
});

tokenEyeBtn.addEventListener("click", () => {
  apiTokenInput.type = apiTokenInput.type === "password" ? "text" : "password";
  tokenEyeBtn.textContent = apiTokenInput.type === "password" ? "👁️" : "🙈";
});

clientSelect.addEventListener("change", refreshConfig);

async function saveTunnelConfig() {
  try {
    const status = await invoke("set_tunnel_config", {
      provider: providerSelect.value,
      authtoken: ngrokToken.value,
      domain: ngrokDomain.value,
    });
    updateUI(status);
    toast("Configuração do túnel salva!");
  } catch (err) {
    errorMsg.textContent = "Erro ao salvar túnel: " + err;
    errorMsg.classList.remove("hidden");
  }
}

providerSelect.addEventListener("change", () => {
  refreshProviderUI();
  saveTunnelConfig();
});

saveTunnelBtn.addEventListener("click", saveTunnelConfig);

ngrokEye.addEventListener("click", () => {
  ngrokToken.type = ngrokToken.type === "password" ? "text" : "password";
  ngrokEye.textContent = ngrokToken.type === "password" ? "👁️" : "🙈";
});

bgToggle.addEventListener("change", async () => {
  try {
    await invoke("set_background_mode", { enabled: bgToggle.checked });
  } catch (err) {
    console.error(err);
  }
});

autostartToggle.addEventListener("change", async () => {
  try {
    await invoke("set_autostart", { enabled: autostartToggle.checked });
  } catch (err) {
    console.error(err);
  }
});

async function copyText(text, btn) {
  if (!text) {
    toast("Nada para copiar ainda.");
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
  toast("Copiado!");
  if (btn) {
    const orig = btn.textContent;
    btn.textContent = "Copiado!";
    setTimeout(() => {
      btn.textContent = orig;
    }, 1500);
  }
}

document.querySelectorAll(".btn-copy[data-target]").forEach((btn) => {
  btn.addEventListener("click", () => {
    const targetId = btn.getAttribute("data-target");
    const input = document.getElementById(targetId);
    copyText(input.value, btn);
  });
});

copyConfigBtn.addEventListener("click", () => copyText(configOutput.value, copyConfigBtn));

// Mantem a interface sincronizada com o estado do Rust.
setInterval(async () => {
  if (!hasTauri) return;
  try {
    const status = await invoke("get_status");
    updateUI(status);
  } catch {
    // silencioso: o estado sera tentado novamente no proximo ciclo
  }
}, 10000);

init();
