const key = location.hash.slice(1) || sessionStorage.getItem("zxs-prepare-key");
export const hasAccess = Boolean(key);

if (location.hash && key) {
  sessionStorage.setItem("zxs-prepare-key", key);
  history.replaceState(null, "", location.pathname);
}

export async function api(action) {
  if (!key) throw new Error("请打开终端显示的本机链接。");
  const response = await fetch("/api", {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Zxs-Key": key },
    body: JSON.stringify(action),
  });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error);
  return data;
}
