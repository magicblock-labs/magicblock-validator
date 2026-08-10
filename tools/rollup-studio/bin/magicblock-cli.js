#!/usr/bin/env node

/**
 * MagicBlock CLI
 */

import { defaultEphemeralValidator } from '../src/core/ephemeral-validator.js';
import { defaultSettlementEngine } from '../src/core/settlement.js';

const args = process.argv.slice(2);
const command = args[0] || 'help';

async function main() {
  switch (command.toLowerCase()) {
    case 'tic': {
      console.log('\n⚡ Executing Sub-10ms Ephemeral Rollup Tic on MagicBlock SVM Validator...');
      const block = defaultEphemeralValidator.executeBlockTic();
      console.log(`  Slot:          #${block.slot}`);
      console.log(`  Block Hash:    ${block.blockHash}`);
      console.log(`  Transactions:  ${block.txCount} txs`);
      console.log(`  Latency:       ${block.latencyMs}`);
      console.log(`  Gas Cost:      ${block.gasSpent}\n`);
      break;
    }

    case 'commit': {
      const accountPubkey = args[1] || 'GameWorldState1111111111111111111111111111';
      console.log(`\n🔐 Committing Ephemeral State for '${accountPubkey}' to Solana L1...`);
      const set = defaultSettlementEngine.commitStateToSolanaL1({ accountPubkey });
      console.log(`  Solana TX:     ${set.solanaTxSignature}`);
      console.log(`  Status:        ${set.settlementStatus}`);
      console.log(`  State Root:    ${set.stateRoot}\n`);
      break;
    }

    case 'studio': {
      console.log('\n🌐 Launching MagicBlock Studio on :3424...');
      await import('../src/server/app.js');
      break;
    }

    default: {
      console.log(`
╔══════════════════════════════════════════════════════════════════╗
║               ⚡ MAGICBLOCK EPHEMERAL ROLLUPS CLI                ║
║    Sub-10ms SVM Execution & Solana L1 Settlement Suite           ║
╚══════════════════════════════════════════════════════════════════╝

Commands:
  magicblock-cli tic                    Execute sub-10ms Ephemeral SVM Rollup block tic
  magicblock-cli commit [pubkey]        Commit ephemeral state to Solana L1 mainnet
  magicblock-cli studio                 Launch Interactive Web Studio on :3424
      `);
      break;
    }
  }
}

main().catch(err => {
  console.error('Error:', err.message);
  process.exit(1);
});
