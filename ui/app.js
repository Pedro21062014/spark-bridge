const { invoke } = window.__TAURI__.core;

const toggleBtn = document.getElementById("toggle-btn");
const btnText = document.getElementById("btn-text");
const statusBadge = document.getElementById("status-badge");
const publicUrlInput = document.getElementById("public-url");
const apiTokenInput = document.getElementById("api-token");
const bgToggle = document.getElementById("bg-toggle");
const autostartToggle = document.getElementById("autostart-toggle");
const errorMsg = document.getElementById("error-msg");

let isRunning = false;

function updateUI(status) {
  isRunning = status.running;
  apiTokenInput.value = status.token || "";
  bgToggle.checked = status.background_mode;

  if (status.running) {
    statusBadge.textContent = "Online";
    statusBadge.className = "badge online";
    btnText.textContent = "Parar";
    toggleBtn.classList.add("stop");
    publicUrlInput.value = status.public_url || "";
  } else {
    statusBadge.textContent = "Desconectado";
    statusBadge.className = "badge offline";
    btnText.textContent = "Iniciar";
    toggleBtn.classList.remove("stop");
    publicUrlInput.value = "";
  }
}

async function init() {
  try {
    const status = await invoke("get_status");
    updateUI(status);
    const autostart = await invoke("get_autostart");
    autostartToggle.checked = autostart;
  } catch (err) {
    console.error("Init error:", err);
  }
}

toggleBtn.addEventListener("click", async () => {
  errorMsg.classList.add("hidden");
  toggleBtn.disabled = true;

  try {
    if (!isRunning) {
      statusBadge.textContent = "Conectando túnel...";
      statusBadge.className = "badge loading";
      btnText.textContent = "Iniciando...";
      const status = await invoke("start_bridge");
      updateUI(status);
    } else {
      btnText.textContent = "Parando...";
      const status = await invoke("stop_bridge");
      updateUI(status);
    }
  } catch (err) {
    errorMsg.textContent = "Erro: " + err;
    errorMsg.classList.remove("hidden");
    statusBadge.textContent = "Desconectado";
    statusBadge.className = "badge offline";
    btnText.textContent = "Iniciar";
    toggleBtn.classList.remove("stop");
  } finally {
    toggleBtn.disabled = false;
  }
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

document.querySelectorAll(".btn-copy").forEach((btn) => {
  btn.addEventListener("click", async () => {
    const targetId = btn.getAttribute("data-target");
    const input = document.getElementById(targetId);
    if (!input.value) return;
    await navigator.clipboard.writeText(input.value);
    const orig = btn.textContent;
    btn.textContent = "Copiado!";
    setTimeout(() => {
      btn.textContent = orig;
    }, 1500);
  });
});

init();
