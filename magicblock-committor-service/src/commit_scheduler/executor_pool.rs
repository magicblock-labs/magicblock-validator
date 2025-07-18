// TODO: how executiong works?
// case - No available worker
// We can't process any messages - waiting

// Say worker finished
// Messages still blocked by each other
// We move to get message from channel
// We stuck
// If we get worker without

// Flow:
// 1. check if there's available message to be executed
// 2. Fetch it and wait for available worker to execute it

// If no messages workers are idle
// If more tham one free, then we will launch woker and pick another
// on next iteration
