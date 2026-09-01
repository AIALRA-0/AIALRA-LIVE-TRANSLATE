package online.aialra.capture

import org.json.JSONObject

/** Only a server commit proof may release a frame from the phone's durable outbox. */
internal fun isDurableAudioAck(message: JSONObject): Boolean {
    return isDurableAudioAck(
        type = message.optString("type"),
        sequence = message.optLong("sequence", -1L),
        commitId = message.optString("commit_id"),
    )
}

/** The primitive overload keeps the ACK contract independently testable on the JVM. */
internal fun isDurableAudioAck(type: String?, sequence: Long, commitId: String?): Boolean {
    return type == "audio.ack" && sequence > 0L && !commitId.isNullOrBlank()
}
