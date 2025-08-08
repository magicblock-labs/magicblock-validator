use crate::{
    consts::{MAX_INSTRUCTION_DATA_SIZE, MAX_INSTRUCTION_LENGTH},
    instruction::{IX_INIT_SIZE, IX_REALLOC_SIZE},
};

/// Creates chunks of realloc instructions such that each chunk fits into a single transaction.
/// - reallocs: The realloc instructions to split up
/// - init_ix: The init instruction that is combined with the first reallocs
pub fn chunk_realloc_ixs<T: Clone>(
    reallocs: Vec<T>,
    init_ix: Option<T>,
) -> Vec<Vec<T>> {
    fn add_reallocs<T: Clone>(
        chunk: &mut Vec<T>,
        reallocs: &mut Vec<T>,
        start_size: u16,
    ) {
        let mut total_size = start_size;
        while total_size + IX_REALLOC_SIZE < MAX_INSTRUCTION_DATA_SIZE
            && chunk.len() < MAX_INSTRUCTION_LENGTH as usize
        {
            if let Some(realloc) = reallocs.pop() {
                chunk.push(realloc);
                total_size += IX_REALLOC_SIZE;
            } else {
                return;
            }
        }
    }

    let mut reallocs = reallocs;
    // We add to the chunks by popping from the end and in order to retain the order
    // of reallocs we reverse them here first
    reallocs.reverse();

    let mut chunks = vec![];

    // First chunk combines reallocs with init instruction if present
    if let Some(init_ix) = init_ix {
        let mut chunk = vec![init_ix];
        add_reallocs(&mut chunk, &mut reallocs, IX_INIT_SIZE);
        chunks.push(chunk);
    }

    // All remaining chunks are pure realloc instructions
    while let Some(realloc) = reallocs.pop() {
        let mut chunk = vec![realloc];
        add_reallocs(&mut chunk, &mut reallocs, IX_REALLOC_SIZE);
        chunks.push(chunk);
    }

    chunks
}
