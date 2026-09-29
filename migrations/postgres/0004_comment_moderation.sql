-- Preserve historical manual approvals when enabling first-comment moderation.
ALTER TABLE comments ADD COLUMN moderation_reason text;
UPDATE comments SET moderation_reason = CASE status
    WHEN 'approved' THEN 'manual_approval'
    WHEN 'pending' THEN 'manual_review'
    WHEN 'spam' THEN 'spam'
    WHEN 'trash' THEN 'trash'
END;
CREATE INDEX comments_manual_approval_user_idx ON comments (user_id)
    WHERE user_id IS NOT NULL AND status = 'approved' AND moderation_reason = 'manual_approval';
