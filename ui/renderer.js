const composer = document.querySelector(".composer");
const prompt = document.querySelector("#prompt");

composer?.addEventListener("submit", (event) => {
  event.preventDefault();
  prompt?.focus();
});
