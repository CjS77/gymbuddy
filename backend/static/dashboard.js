// ── Helpers ───────────────────────────────────────────────────────────────────

async function api(path) {
    const res = await fetch(path);
    if (res.status === 401) { window.location = '/login'; return null; }
    if (!res.ok) throw new Error(`API error: ${res.status}`);
    return res.json();
}

function esc(s) {
    const el = document.createElement('span');
    el.textContent = s;
    return el.innerHTML;
}

function fmtDate(s) {
    if (!s) return '-';
    const d = new Date(s.replace(' ', 'T') + 'Z');
    return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
}

function fmtVolume(v) {
    if (v >= 1000) return (v / 1000).toFixed(1) + 'k';
    return Math.round(v).toString();
}

// ── Dashboard page ───────────────────────────────────────────────────────────

async function loadDashboard() {
    const [goals, health, logs] = await Promise.all([
        api('/api/goals'),
        api('/api/health'),
        api('/api/sets?limit=20'),
    ]);

    // Week stats — derive from logs
    const el = (id) => document.getElementById(id);

    // Goals
    const goalsList = el('goals-list');
    if (goals && goals.length > 0) {
        goalsList.innerHTML = goals.map(g => {
            const pct = Math.min(100, Math.round(g.percentage));
            const cls = g.status === 'achieved' ? ' achieved' : '';
            return `<div class="goal-item">
                <div class="goal-header"><span>${esc(g.exercise_name)}</span><span>${pct}%</span></div>
                <div class="goal-bar"><div class="goal-fill${cls}" style="width:${pct}%"></div></div>
            </div>`;
        }).join('');
    } else {
        goalsList.innerHTML = '<p class="placeholder">No active goals</p>';
    }

    // Health
    const healthList = el('health-list');
    if (health && health.length > 0) {
        healthList.innerHTML = health.map(h => {
            const badge = `badge-${h.entry_type}`;
            const body = h.body_part || 'general';
            return `<div class="health-item">
                <span class="badge ${badge}">${esc(h.entry_type)}</span>
                <span>${esc(body)}: ${esc(h.description)}</span>
            </div>`;
        }).join('');
    } else {
        healthList.innerHTML = '<p class="placeholder">No active health issues</p>';
    }

    // Recent sessions (derive from logs data)
    const sessionsList = el('sessions-list');
    if (logs && logs.data && logs.data.length > 0) {
        const byDate = {};
        logs.data.forEach(l => {
            const d = l.logged_at.split(' ')[0];
            byDate[d] = (byDate[d] || 0) + 1;
        });
        const dates = Object.entries(byDate).slice(0, 5);
        sessionsList.innerHTML = dates.map(([d, count]) =>
            `<div class="session-item"><span>${fmtDate(d)}</span><span>${count} exercises</span></div>`
        ).join('');
    } else {
        sessionsList.innerHTML = '<p class="placeholder">No recent sessions</p>';
    }
}

// ── History page ─────────────────────────────────────────────────────────────

let historyState = { offset: 0, limit: 50 };

function initHistory(exercises) {
    const sel = document.getElementById('filter-exercise');
    exercises.forEach(e => {
        const opt = document.createElement('option');
        opt.value = e.id; opt.textContent = e.name;
        sel.appendChild(opt);
    });

    document.getElementById('btn-apply').addEventListener('click', () => {
        historyState.offset = 0;
        loadLogs(exercises);
    });
    document.getElementById('btn-export').addEventListener('click', () => exportCSV());

    loadLogs(exercises);
}

async function loadLogs(exercises) {
    const from = document.getElementById('filter-from').value;
    const to = document.getElementById('filter-to').value;
    const exId = document.getElementById('filter-exercise').value;

    let url = `/api/sets?limit=${historyState.limit}&offset=${historyState.offset}`;
    if (from) url += `&from=${from}`;
    if (to) url += `&to=${to}`;
    if (exId) url += `&exercise_type_id=${exId}&include_descendants=true`;

    const data = await api(url);
    if (!data) return;

    const exMap = {};
    exercises.forEach(e => { exMap[e.id] = e.name; });

    const tbody = document.getElementById('logs-body');
    if (data.data.length === 0) {
        tbody.innerHTML = '<tr><td colspan="7" class="placeholder">No sets found</td></tr>';
    } else {
        tbody.innerHTML = data.data.map(s => {
            const name = exMap[s.exercise_type_id] || `#${s.exercise_type_id}`;
            let countCell = s.count != null ? `${s.count}` : '-';
            let valueCell = '-';
            switch (s.measurement_type) {
                case 'weight_reps': valueCell = `${s.value} kg`; break;
                case 'time_based': valueCell = `${s.value}s`; break;
                case 'distance_based': valueCell = `${s.value} m`; break;
                case 'level_based': valueCell = `lvl ${s.value}`; break;
                case 'score_based': valueCell = `${s.value}`; break;
            }
            return `<tr>
                <td>${fmtDate(s.logged_at)}</td>
                <td>${esc(name)}</td>
                <td>${countCell}</td>
                <td>${valueCell}</td>
                <td>${esc(s.measurement_type)}</td>
                <td>${esc(s.perceived_difficulty)}</td>
                <td>${s.comment ? esc(s.comment) : '-'}</td>
            </tr>`;
        }).join('');
    }

    // Pagination
    const pagDiv = document.getElementById('pagination');
    const totalPages = Math.ceil(data.total / data.limit);
    const currentPage = Math.floor(data.offset / data.limit);
    let html = '';
    if (totalPages > 1) {
        html += `<button ${currentPage === 0 ? 'disabled' : ''} onclick="historyPage(${currentPage - 1})">Prev</button>`;
        for (let i = 0; i < Math.min(totalPages, 10); i++) {
            html += `<button class="${i === currentPage ? 'active' : ''}" onclick="historyPage(${i})">${i + 1}</button>`;
        }
        html += `<button ${currentPage >= totalPages - 1 ? 'disabled' : ''} onclick="historyPage(${currentPage + 1})">Next</button>`;
    }
    pagDiv.innerHTML = html;

    window._historyExercises = exercises;
}

function historyPage(page) {
    historyState.offset = page * historyState.limit;
    loadLogs(window._historyExercises);
}

function exportCSV() {
    const table = document.getElementById('logs-table');
    const rows = Array.from(table.querySelectorAll('tr'));
    const csv = rows.map(r =>
        Array.from(r.querySelectorAll('th, td')).map(c => `"${c.textContent.replace(/"/g, '""')}"`).join(',')
    ).join('\n');
    const blob = new Blob([csv], { type: 'text/csv' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = 'exercise_history.csv';
    a.click();
}

// ── Progress page ────────────────────────────────────────────────────────────

let charts = {};

function initProgress(exercises) {
    const sel = document.getElementById('exercise-select');
    exercises.forEach(e => {
        const opt = document.createElement('option');
        opt.value = e.id; opt.textContent = `${e.name} (${e.muscle_group})`;
        sel.appendChild(opt);
    });

    sel.addEventListener('change', loadWeightChart);
    document.getElementById('time-range').addEventListener('change', loadWeightChart);
    document.getElementById('volume-weeks').addEventListener('change', loadVolumeChart);
    document.getElementById('freq-weeks').addEventListener('change', loadFrequencyChart);

    loadVolumeChart();
    loadFrequencyChart();
    loadRecords();
}

async function loadWeightChart() {
    const exId = document.getElementById('exercise-select').value;
    if (!exId) return;
    const days = parseInt(document.getElementById('time-range').value);
    const from = new Date(Date.now() - days * 86400000).toISOString().split('T')[0];

    const data = await api(`/api/progress/exercise?exercise_type_id=${exId}&include_descendants=true&from=${from}`);
    if (!data) return;

    if (charts.weight) charts.weight.destroy();
    const ctx = document.getElementById('weight-chart').getContext('2d');
    charts.weight = new Chart(ctx, {
        type: 'line',
        data: {
            labels: data.map(p => p.date),
            datasets: [{ label: 'Best per day', data: data.map(p => p.value),
                         borderColor: '#4361ee', backgroundColor: '#4361ee22', fill: true, tension: 0.3 }]
        },
        options: { responsive: true, maintainAspectRatio: false,
                   scales: { x: { display: true }, y: { beginAtZero: false } } }
    });
}

async function loadVolumeChart() {
    const weeks = document.getElementById('volume-weeks').value;
    const data = await api(`/api/progress/volume?weeks=${weeks}`);
    if (!data) return;

    // Group by week, create datasets per muscle group
    const weeks_set = [...new Set(data.map(d => d.week))].sort();
    const groups = [...new Set(data.map(d => d.muscle_group))];
    const colors = ['#4361ee','#06d6a0','#ffd166','#ef476f','#118ab2','#073b4c','#8338ec','#ff6b6b',
                    '#48cae4','#95d5b2','#f8961e','#577590','#43aa8b','#f94144','#90be6d','#277da1'];

    const datasets = groups.map((g, i) => ({
        label: g,
        data: weeks_set.map(w => { const item = data.find(d => d.week === w && d.muscle_group === g); return item ? item.total_volume : 0; }),
        backgroundColor: colors[i % colors.length],
    }));

    if (charts.volume) charts.volume.destroy();
    const ctx = document.getElementById('volume-chart').getContext('2d');
    charts.volume = new Chart(ctx, {
        type: 'bar',
        data: { labels: weeks_set, datasets },
        options: { responsive: true, maintainAspectRatio: false,
                   scales: { x: { stacked: true }, y: { stacked: true, beginAtZero: true } } }
    });
}

async function loadFrequencyChart() {
    const weeks = document.getElementById('freq-weeks').value;
    const data = await api(`/api/progress/frequency?weeks=${weeks}`);
    if (!data) return;

    if (charts.freq) charts.freq.destroy();
    const ctx = document.getElementById('frequency-chart').getContext('2d');
    charts.freq = new Chart(ctx, {
        type: 'bar',
        data: {
            labels: data.map(d => d[0]),
            datasets: [{ label: 'Sessions', data: data.map(d => d[1]),
                         backgroundColor: '#4361ee' }]
        },
        options: { responsive: true, maintainAspectRatio: false,
                   scales: { y: { beginAtZero: true, ticks: { stepSize: 1 } } } }
    });
}

async function loadRecords() {
    const data = await api('/api/progress/records');
    if (!data) return;

    const tbody = document.getElementById('records-body');
    if (data.length === 0) {
        tbody.innerHTML = '<tr><td colspan="4" class="placeholder">No records yet</td></tr>';
        return;
    }
    tbody.innerHTML = data.map(r => {
        let val = r.value;
        if (r.measurement_type === 'weight_reps') val += ' kg';
        else if (r.measurement_type === 'time_based') val += 's';
        else if (r.measurement_type === 'distance_based') val += ' m';
        const mg = r.muscle_group || '-';
        return `<tr><td>${esc(r.exercise_name)}</td><td>${esc(mg)}</td><td>${val}</td><td>${fmtDate(r.achieved_at)}</td></tr>`;
    }).join('');
}

// ── Chat page ────────────────────────────────────────────────────────────────

function initChat() {
    loadChatHistory();

    document.getElementById('chat-form').addEventListener('submit', async (e) => {
        e.preventDefault();
        const input = document.getElementById('chat-text');
        const msg = input.value.trim();
        if (!msg) return;

        appendBubble('user', msg);
        input.value = '';
        input.disabled = true;

        const typing = document.createElement('div');
        typing.className = 'chat-typing';
        typing.textContent = 'Thinking...';
        document.getElementById('chat-messages').appendChild(typing);
        scrollChat();

        try {
            const res = await fetch('/api/chat', {
                method: 'POST',
                headers: { 'Content-Type': 'application/json' },
                body: JSON.stringify({ message: msg }),
            });
            typing.remove();

            if (res.status === 429) {
                appendBubble('assistant', 'Rate limit reached. Please wait a moment.');
            } else if (res.ok) {
                const data = await res.json();
                appendBubble('assistant', data.reply);
            } else {
                appendBubble('assistant', 'Something went wrong. Please try again.');
            }
        } catch {
            typing.remove();
            appendBubble('assistant', 'Connection error. Please try again.');
        }

        input.disabled = false;
        input.focus();
    });
}

async function loadChatHistory() {
    const data = await api('/api/chat/history?limit=50');
    if (!data) return;
    data.forEach(m => appendBubble(m.role, m.content));
    scrollChat();
}

function appendBubble(role, text) {
    const div = document.createElement('div');
    div.className = `chat-bubble ${role}`;
    div.textContent = text;
    document.getElementById('chat-messages').appendChild(div);
    scrollChat();
}

function scrollChat() {
    const el = document.getElementById('chat-messages');
    el.scrollTop = el.scrollHeight;
}
