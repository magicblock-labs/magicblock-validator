# Development Governance Manifesto

Owner: Babur Makhmudov, Gabriele Picco
Tags: Policies, governance
Created time: November 16, 2025 2:57 PM

### 1. Guiding Principles

This framework’s purpose is to balance development velocity with high-quality standards. It is guided by these principles:

- **Velocity**: Prioritize shipping value by minimizing time spent in deadlocked debates.
- **Clarity & Accountability**: Eliminate ambiguity with clearly defined roles and responsibilities.
- **Quality & Cohesion**: Maintain a consistent architectural vision through empowered, accountable owners.
- **Fair Process**: Ensure no single authority is absolute by providing a structured path for resolving disputes and ratifying significant changes.

---

### 2. On Professional Conduct & Disagreements

This framework is designed to facilitate healthy, professional debate. All discussions, especially during disputes, must be guided by the following principles:

**Critique ideas, not people.** Arguments must be directed at the technical approach, code, or design, never at an individual. Assume good intent from all participants. A challenge to a technical decision is not a personal attack; it is an attempt to find the best outcome for the project.

Once a decision is reached, it is final. All team members are expected to **disagree and commit**—meaning even if you argued for a different path during the debate, you must fully support the execution of the final decision.

---

### 3. Categories of Decisions & Grounds for Conflict

To provide a clear basis for discussion, decisions and potential conflicts are classified into three categories. Each category maps to a specific resolution path within our governance model.

1. **High-Level System Architecture**: Decisions concerning the fundamental structure of the system, cross-crate interactions, major dependencies, and core infrastructure.
2. **Implementation Details**: The specific logic, algorithms, and methods used to solve a problem *within the scope of a single crate*.
3. **Code Style & Patterns**: Choices related to formatting, naming conventions, and common patterns, guided by consistency with the existing codebase and global standards.

---

### 4. The Code Ownership Model

Clear ownership is the foundation of this framework. It ensures every part of the codebase has a designated steward.

### The Unit of Ownership

The fundamental unit of ownership is the **Cargo Crate**.

### The Code Ownership Manifest

A `CODEOWNERS.md` file in the repository root serves as the definitive source of truth for ownership.

### Becoming a Code Owner

1. **New Crates (Default)**: The primary author designates themselves as the CO in the initial PR.
2. **Unowned Crates (Claim)**: Any team member can claim an unowned crate via a PR with at least one approval.
3. **Transfer of Ownership (Exceptional)**: Transfers require a formal proposal and vote by the Consensus Council.

### Responsibilities & Authority of the Code Owner

The CO is the steward for their crate(s), responsible for quality and consistency. Their approval is necessary to merge a PR. They hold the **Steward's Prerogative** to make final decisions on **Implementation Details** and **Code Style** within their crate to break deadlocks.

---

### 5. The Decision-Making & Escalation Framework

This framework establishes a clear, three-tiered structure for making decisions.

### Tier 1: The Code Owner (Default Authority)

- **Domain**: **Implementation Details** and **Code Style & Patterns** within their owned crate.
- **Process**: The CO has the final decision-making authority.

### Tier 2: The Consensus Council (Dispute & Architectural Governance)

- **Domain**: **High-Level System Architecture**, proposals for new **Global Code Styles**, and disputed Tier 1 decisions.
- **Process**: The Council is the primary body for resolving disputes and ratifying significant, team-wide changes.

### Tier 3: The CTO (Final Tie-Breaker)

- **Domain**: Deadlocked Council votes.
- **Process**: In the rare event the Council cannot reach a supermajority, the **CTO will cast the final, tie-breaking vote**.

---

### 6. The Consensus Council Protocol

- **Formation**: The Council is formed on-demand from all members of the Labs team.
- **Timeboxing**: Votes are time-boxed to **24 hours**. This can be extended by a simple majority vote.
- **Supermajority Rule**: A motion passes only if it receives affirmative votes from **two-thirds (2/3) of the entire Council membership**. Abstention or non-participation counts against the motion's success.

---

### 7. Standard Workflows

### 7.1. Design Consultation

Before implementing significant changes, the assignee must consult with the CO of the affected crate(s) to align on the proposed design and impact. For minor changes, a brief message suffices.

### 7.2. PR Review Process

1. **Initiation**: The PR author notifies the relevant CO(s) upon opening a PR.
2. **Review**: The CO prioritizes the review. If a discussion reaches an impasse, the CO is empowered to use the **Steward's Prerogative**.
3. **Escalation (Optional)**: If a reviewer or author strongly disagrees with the CO's decision, they may formally escalate the matter to the **Consensus Council**.
4. **Resolution**: The Council resolves the dispute within 24 hours. The outcome is final.

