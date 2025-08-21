# Architecture of Intent Execution
Here the flow of Intent Execution is explained. After its creation by User it
makes it to CommittorService. The main responsibility of CommittorService - execution of Intents.


Due to blocking nature of intents, where one intent can block execution of another,
we introduce **Schedulers**

## Schedulers
We can't directly spawn a bunch of **IntentExecutor**s. The reason is that one message can block execution of another message. To handle this the messages have to go through Scheduling.

Details: Once message make it to `CommittorProcessor::schedule_base_intents` it outsources intent to tokio task `IntentExecutionEngine` which figures out a scheduling.

## IntentExecutionEngine
Accepts new messages, schedules them, and spawns up to `MAX_EXECUTORS`(50) parallel **IntentExecutor**s for each Intent. Once a particular **IntentExecutor** finishes execution we broadcast result to subscribers, like: `RemoteScheduledCommitsProcessor` or `ExternalAccountsManager`

Details: For scheduling logic see **IntentScheduler**.  Number of parallel **IntentExecutor** is controller by Semaphore.

## IntentExecutor
IntentExecutor - responsible for execution of Intent. Calls  **TransactionPreparator** and then executes a transaction returning as result necessary signatures

## TransactionPreparator
TransactionPreparator - is an entity that handles all of the above "Transaction preparation" calling **TaskBuilderV1**,  **TaskStrategist**, **DeliveryPreparator** and then assempling it all and passing to **MessageExecutor**

## DeliveryPreparator
After our **L1Task**s are ready we need to prepare eveything for their successful execution. **DeliveryPreparator** - handles ALTs and commit buffers

## TaskBuilder
First, lets build atomic tasks from scheduled message/intent.

High level: TaskBuilder responsible for creating L1Tasks(to be renamed...) from ScheduledL1Message(to be renamed...).
Details: To do that is requires additional information from DelegationMetadata, it is provided **CommitIdFetcher**

### BaseTask
High level: BaseTask - is an atomic operation that is to be performed on the Base layer, like: Commit, Undelegate, Finalize, Action.

Details: There's to implementation of BaseTask: ArgsTask, BufferTask. ArgsTask - gives instruction using args. BufferTask - gives instruction using buffer. BufferTask at the moment supports only commits

### TaskInfoFetcher
High level: for account to be accepted by `dlp` it needs to have incremental commit ids. TaskInfoFetcher provides a user with the correct ids/nonces for set of committees

Details: CacheTaskInfoFetcher - implementation of TaskInfoFetcher, that caches and locally increments commit ids using LruCache

## TaskStrategist
After our tasks were built with **TaskBuilder**, they need to be optimized to fit into transaction. That what TaskStrategist does.

Details: Initially **TaskBuilder** builds ArgsTasks,  **TaskStrategist** if needed optimzes them to BufferTask.
