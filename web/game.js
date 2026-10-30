const play = document.querySelector("#play");
const status = document.querySelector("#status");
const launcher = document.querySelector("#launcher");
const canvas = document.querySelector("#game-canvas");

function fail(error) {
  console.error(error);
  launcher.hidden = false;
  status.textContent =
    "The game could not start. Check that WebGL2 is enabled, then reload to try again.";
  play.disabled = false;
  play.textContent = "Reload";
  play.onclick = () => location.reload();
}

try {
  const { default: init, start_game } = await import("./pkg/gravity_spy.js");
  await init();
  status.textContent = "Ready when you are.";
  play.disabled = false;
  play.onclick = () => {
    play.disabled = true;
    try {
      start_game();
      launcher.hidden = true;
      canvas.focus();
    } catch (error) {
      fail(error);
    }
  };
  window.addEventListener("error", (event) => fail(event.error ?? event.message));
  window.addEventListener("unhandledrejection", (event) => fail(event.reason));
} catch (error) {
  fail(error);
}
