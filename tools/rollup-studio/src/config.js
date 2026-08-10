/**
 * MagicBlock Ephemeral Rollup (ER) Configuration
 */

export const MAGICBLOCK_CONFIG = {
  architecture: {
    name: 'MagicBlock Ephemeral Rollups (ER)',
    runtime: 'Solana Virtual Machine (SVM)',
    targetLatencyMs: 10,
    settlementLayer: 'Solana L1 Mainnet-Beta',
    delegationProgram: 'magicblock-delegation-program',
  },
  rollupCapabilities: [
    'Sub-10ms Real-Time SVM Block Tic Execution',
    'Solana State Account Delegation & Lock Engine',
    'Gasless High-Frequency Transaction Processing',
    'Optimistic State Commitment & Rollback Verification',
  ],
  sampleDelegations: [
    {
      accountPubkey: 'GameWorldState1111111111111111111111111111',
      ownerProgram: 'BoltGameEngine11111111111111111111111111',
      delegatedTo: 'MagicBlockValidatorAlpha',
      status: 'DELEGATED_EPHEMERAL_ACTIVE',
    },
    {
      accountPubkey: 'HftOrderbookState22222222222222222222222',
      ownerProgram: 'MagicDexRouter22222222222222222222222222',
      delegatedTo: 'MagicBlockValidatorBeta',
      status: 'DELEGATED_EPHEMERAL_ACTIVE',
    },
  ],
};
