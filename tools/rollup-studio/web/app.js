/**
 * MagicBlock Studio Client Logic
 */

let isAutoStreaming = false;
let autoInterval = null;

document.addEventListener('DOMContentLoaded', () => {
  initTabs();
  loadConfig();
  initListeners();
});

function initTabs() {
  const tabs = document.querySelectorAll('.nav-tab');
  tabs.forEach(tab => {
    tab.addEventListener('click', () => {
      document.querySelectorAll('.nav-tab').forEach(t => t.classList.toggle('active', t === tab));
      document.querySelectorAll('.tab-pane').forEach(p => p.classList.toggle('active', p.id === `tab-${tab.dataset.tab}`));
    });
  });
}

async function loadConfig() {
  try {
    const res = await fetch('/api/config');
    const data = await res.json();

    const select = document.getElementById('select-account');
    select.innerHTML = '';

    data.delegations.forEach(d => {
      const opt = document.createElement('option');
      opt.value = d.accountPubkey;
      opt.textContent = `${d.accountPubkey} (${d.status})`;
      select.appendChild(opt);
    });
  } catch (e) {
    console.error(e);
  }
}

function initListeners() {
  document.getElementById('btn-tic').addEventListener('click', executeTic);

  const autoBtn = document.getElementById('btn-toggle-auto');
  autoBtn.addEventListener('click', () => {
    if (isAutoStreaming) {
      clearInterval(autoInterval);
      isAutoStreaming = false;
      autoBtn.textContent = '▶️ Start Real-Time Stream (20 tics/sec • 10ms)';
      autoBtn.className = 'btn btn-gradient btn-lg';
    } else {
      isAutoStreaming = true;
      autoBtn.textContent = '⏸️ Pause Sub-10ms Stream';
      autoBtn.className = 'btn btn-secondary btn-lg';
      executeTic();
      autoInterval = setInterval(executeTic, 50); // 50ms fast stream
    }
  });

  document.getElementById('settlement-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const accountPubkey = document.getElementById('select-account').value;
    const box = document.getElementById('settlement-json-box');

    try {
      const res = await fetch('/api/settlement/commit', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ accountPubkey }),
      });
      const data = await res.json();
      box.textContent = JSON.stringify(data, null, 2);
    } catch (err) {
      box.textContent = `Error: ${err.message}`;
    }
  });
}

async function executeTic() {
  try {
    const res = await fetch('/api/rollup/tic', { method: 'POST' });
    const data = await res.json();
    if (data.success) {
      appendBlockRow(data.block);
    }
  } catch (e) {
    console.warn(e);
  }
}

function appendBlockRow(block) {
  const container = document.getElementById('blocks-container');
  const empty = container.querySelector('.empty-state');
  if (empty) container.innerHTML = '';

  const row = document.createElement('div');
  row.className = 'ledger-row';
  row.innerHTML = `
    <div>
      <div style="font-weight: 700; color: #fff;">SVM Slot #${block.slot.toLocaleString()}</div>
      <div class="mono text-muted" style="font-size: 0.72rem;">StateRoot: ${block.stateRoot.slice(0, 14)}...</div>
    </div>
    <div style="text-align: right;">
      <div style="color: #9333ea; font-weight: 700; font-family: var(--font-mono);">${block.latencyMs} Tic</div>
      <div class="mono text-muted" style="font-size: 0.72rem;">${block.txCount} txs • ${block.gasSpent}</div>
    </div>
  `;
  container.insertBefore(row, container.firstChild);
}
