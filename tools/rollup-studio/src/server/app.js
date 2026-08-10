/**
 * MagicBlock Ephemeral Rollup Studio Web Server
 */

import express from 'express';
import cors from 'cors';
import path from 'path';
import { fileURLToPath } from 'url';
import { MAGICBLOCK_CONFIG } from '../config.js';
import { defaultEphemeralValidator } from '../core/ephemeral-validator.js';
import { defaultSettlementEngine } from '../core/settlement.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const WEB_ROOT = path.join(__dirname, '../../web');

const app = express();
const PORT = process.env.PORT || 3424;

app.use(cors());
app.use(express.json());
app.use(express.static(WEB_ROOT));

// 1. Config & Delegations
app.get('/api/config', (req, res) => {
  res.json({
    architecture: MAGICBLOCK_CONFIG.architecture,
    capabilities: MAGICBLOCK_CONFIG.rollupCapabilities,
    delegations: MAGICBLOCK_CONFIG.sampleDelegations,
  });
});

// 2. Execute Ephemeral Rollup Sub-10ms Tic
app.post('/api/rollup/tic', (req, res) => {
  const block = defaultEphemeralValidator.executeBlockTic();
  res.json({ success: true, block });
});

// 3. Rollup Block History
app.get('/api/rollup/blocks', (req, res) => {
  res.json(defaultEphemeralValidator.getRecentRollups());
});

// 4. Commit State to Solana L1
app.post('/api/settlement/commit', (req, res) => {
  try {
    const result = defaultSettlementEngine.commitStateToSolanaL1(req.body);
    res.json(result);
  } catch (err) {
    res.status(400).json({ error: err.message });
  }
});

if (process.env.NODE_ENV !== 'test') {
  app.listen(PORT, () => {
    console.log(`\n======================================================`);
    console.log(`⚡ MagicBlock Ephemeral Rollup (ER) Studio Running!`);
    console.log(`🌐 Web Dashboard: http://localhost:${PORT}`);
    console.log(`🚀 Execution Latency: ~10ms Sub-Second SVM Tics`);
    console.log(`======================================================\n`);
  });
}

export default app;
