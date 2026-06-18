// ====== 媒体播放视图 ======
import { toast } from '../components/toast.js';
import { escapeHtml } from '../utils.js';

export function playMedia(fullPath, type, encodedName) {
  const token = localStorage.getItem('cloud_auth_token');
  if (!token) return toast('请先登录', 'error');

  const mediaUrl = `/api/media/${encodeURIComponent(fullPath)}?token=${encodeURIComponent(token)}`;
  const name = decodeURIComponent(encodedName);
  const titleEl = document.getElementById('player-title');
  const contentZone = document.getElementById('player-content');
  if (titleEl) titleEl.innerText = `查看: ${name}`;

  if (type === 'image') {
    if (contentZone) contentZone.innerHTML = `<img src="${mediaUrl}" class="max-w-full max-h-[70vh] object-contain rounded-lg shadow-lg" alt="${escapeHtml(name)}">`;
  } else if (type === 'pdf') {
    if (contentZone) contentZone.innerHTML = `<iframe src="${mediaUrl}" class="w-full h-[80vh] border-0 rounded-lg"></iframe>`;
  } else if (type === 'video') {
    if (contentZone) contentZone.innerHTML = `<video src="${mediaUrl}" controls autoplay class="w-full max-h-[60vh] object-contain"></video>`;
  } else if (type === 'audio') {
    if (contentZone) contentZone.innerHTML = `<audio src="${mediaUrl}" controls autoplay class="w-full py-4"></audio>`;
  }
  document.getElementById('player-modal')?.classList.remove('hidden');
}

export function closePlayer() {
  const contentZone = document.getElementById('player-content');
  const video = contentZone?.querySelector('video');
  const audio = contentZone?.querySelector('audio');
  if (video) { video.pause(); video.src = ''; }
  if (audio) { audio.pause(); audio.src = ''; }
  if (contentZone) contentZone.innerHTML = '';
  document.getElementById('player-modal')?.classList.add('hidden');
}
