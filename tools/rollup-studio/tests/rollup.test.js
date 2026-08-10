/**
 * MagicBlock Ephemeral Rollup Unit Tests
 */

import { defaultEphemeralValidator } from '../src/core/ephemeral-validator.js';
import { defaultSettlementEngine } from '../src/core/settlement.js';

async function runRollupTests() {
  console.log('Testing MagicBlock Ephemeral Rollup (ER) Engine...');

  // 1. Execute Sub-10ms Tic
  const block = defaultEphemeralValidator.executeBlockTic();
  if (!block.blockHash || !block.latencyMs) {
    throw new Error('Ephemeral SVM block tic execution failed');
  }

  // 2. Commit Settlement
  const settlement = defaultSettlementEngine.commitStateToSolanaL1({
    accountPubkey: 'GameWorldState1111111111111111111111111111',
  });
  if (!settlement.solanaTxSignature) {
    throw new Error('Solana L1 state settlement failed');
  }

  console.log(`✅ MagicBlock Ephemeral Rollup Tic (${block.latencyMs}) & Solana L1 Settlement Verified!`);
}

runRollupTests().catch(e => {
  console.error('❌ Rollup Test Failed:', e);
  process.exit(1);
});
