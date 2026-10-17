# Error Handling

The NodeGet project API uses a unified error handling mechanism to ensure all RPC calls provide consistent error
response formats.

## Error Response Format

All API errors follow this JSON structure:

```json
{
  "error": {
    "code": <error_code>,
    "message": "<error_message>",
    "data": <optional_error_data>
  }
}
```

## NodegetError Enum

NodeGet defines a unified error enum `NodegetError` with the following error types:

### Error Types

- **ParseError(String)** - Parse Error (Error Code: 101)
    - Thrown when request data parsing fails

- **InvalidInput(String)** - Invalid Input (Error Code: 108)
    - Thrown when request parameters are syntactically valid but semantically invalid

- **PermissionDenied(String)** - Permission Denied (Error Code: 102)
    - Thrown when user doesn't have sufficient permissions to perform the operation

- **DatabaseError(String)** - Database Error (Error Code: 103)
    - Thrown when database operations fail

- **AgentConnectionError(String)** - Agent Connection Error (Error Code: 104)
    - Thrown when connection to agent nodes fails

- **NotFound(String)** - Not Found (Error Code: 105)
    - Thrown when requested resource doesn't exist

- **UuidNotFound(String)** - UUID Not Found (Error Code: 106)
    - Thrown when requested UUID doesn't exist

- **ConfigNotFound(String)** - Config Not Found (Error Code: 107)
    - Thrown when the configuration file cannot be read or parsed

- **AgentOffline(String)** - Agent Offline (Error Code: 109)
    - Thrown when the target agent has no active task subscription (caught by the pre-dispatch check,
      or the agent disconnected between the check and the send)
    - Message looks like `Agent <uuid> is not connected` (no prefix) so clients can match it directly
    - Thrown when requested configuration doesn't exist

- **SerializationError(String)** - Serialization Error (Error Code: 101)
    - Thrown when data serialization/deserialization fails

- **IoError(String)** - IO Error (Error Code: 101)
    - Thrown when input/output operations fail

- **Other(String)** - Other Error (Error Code: 999)
    - Used for other uncategorized errors

## JsonError Structure

To unify error response formats, NodeGet uses the `JsonError` structure:

```rust
pub struct JsonError {
    pub error_id: i128,           // Error code
    pub error_message: String,    // Error message
}
```

### Error Code Mapping

- `101` - ParseError / SerializationError / IoError
- `102` - PermissionDenied
- `103` - DatabaseError
- `104` - AgentConnectionError
- `105` - NotFound
- `106` - UuidNotFound
- `107` - ConfigNotFound
- `108` - InvalidInput
- `999` - Other

## Error Handling Examples

### Success Response

```json
{
  "result": {
    // Successful data
  }
}
```

### Error Response

```json
{
  "error": {
    "code": 102,
    "message": "Permission denied: Insufficient permissions to read requested task types",
    "data": null
  }
}
```

## Error Handling Best Practices

1. All RPC methods should return unified error formats
2. Use appropriate error types to provide clear error information
3. Include sufficient context in error messages for debugging
4. Sensitive information should not be exposed in error messages