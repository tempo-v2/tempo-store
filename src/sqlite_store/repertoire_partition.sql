CREATE TABLE repertoire_partition_lineage (
    dataset_id TEXT NOT NULL,
    destination_target_id TEXT NOT NULL,
    source_target_id TEXT NOT NULL,
    answer_version BLOB NOT NULL CHECK (length(answer_version)=8),
    applied_at_ms INTEGER NOT NULL CHECK (applied_at_ms>=0),
    PRIMARY KEY (dataset_id, destination_target_id),
    CHECK (destination_target_id <> source_target_id),
    FOREIGN KEY (dataset_id,destination_target_id,answer_version)
        REFERENCES content_history(dataset_id,target_id,version),
    FOREIGN KEY (dataset_id,source_target_id,answer_version)
        REFERENCES content_history(dataset_id,target_id,version)
) STRICT;
