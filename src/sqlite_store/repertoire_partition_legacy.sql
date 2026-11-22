-- Only the installed schema-20 release encoded partition lineage in receipts.
-- The first version of a new destination was the source version at cutover.
INSERT INTO repertoire_partition_lineage
SELECT destination.dataset_id, destination.target_id, mapping.value, destination.version,
       json_extract(receipt.reply,'$.applied_at_ms')
FROM content_receipts AS receipt
JOIN json_each(CASE WHEN json_valid(receipt.reply) THEN receipt.reply END,'$.learning_sources') AS mapping
JOIN content_history AS destination ON destination.target_id=mapping.key
WHERE receipt.request GLOB 'opening-partition-v1:*'
  AND substr(receipt.request,1,length('opening-partition-v1:'||destination.dataset_id||':'))
      = 'opening-partition-v1:'||destination.dataset_id||':'
  AND destination.ordinal=(
    SELECT min(history.ordinal) FROM content_history AS history
    WHERE history.dataset_id=destination.dataset_id AND history.target_id=destination.target_id
  );
